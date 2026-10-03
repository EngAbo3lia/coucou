//! NL → query planning for the ERP chat.
//!
//! A question is handed to the configured chat model with a compact slice of the
//! catalogue (the report entity sets, their fields, the relations, and the query
//! rules). The model answers with a strict JSON `Plan`. Nothing from the model is
//! trusted: every field, metric and filter is re-checked against `query` before a
//! request is built, so an LLM can never emit a query that reads a field the
//! report allowlist does not permit.
//!
//! The plan is either ready to run (`kind` = aggregate/list/count) or needs a
//! clarifying answer from the user (`kind` = clarify). When it is ambiguous the
//! assistant asks rather than guessing, which is the whole point of the change.

use serde::{Deserialize, Serialize};

use super::query;
use crate::providers::{self, ApiStyle};
use crate::settings::Settings;

/// A metric the planner wants to compute.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MetricSpec {
    pub field: String,
    /// "sum" | "average" | "count".
    pub op: String,
    pub alias: String,
}

/// A single filter condition.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct FilterSpec {
    pub field: String,
    /// "ge" | "le" | "gt" | "lt" | "eq".
    pub op: String,
    pub value: String,
}

/// A symbolic time window, resolved to real dates by the executor. The model is
/// not trusted to compute dates, only to name the window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimeWindow {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    ThisQuarter,
    LastQuarter,
    ThisYear,
    LastYear,
    #[serde(rename = "last_30_days")]
    Last30Days,
    All,
}

impl TimeWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeWindow::Today => "today",
            TimeWindow::Yesterday => "yesterday",
            TimeWindow::ThisWeek => "this week",
            TimeWindow::LastWeek => "last week",
            TimeWindow::ThisMonth => "this month",
            TimeWindow::LastMonth => "last month",
            TimeWindow::ThisQuarter => "this quarter",
            TimeWindow::LastQuarter => "last quarter",
            TimeWindow::ThisYear => "this year",
            TimeWindow::LastYear => "last year",
            TimeWindow::Last30Days => "the last 30 days",
            TimeWindow::All => "all time",
        }
    }
}

/// What the plan is asking the model to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanKind {
    Aggregate,
    List,
    Count,
    /// Just reply with `summary` — a welcome, a general answer, a non-data reply.
    Answer,
    /// Ambiguous: needs the user to answer `clarifying_question`.
    Clarify,
}

/// The structured plan the model returns, after validation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Plan {
    /// "aggregate" | "list" | "count" | "answer" | "clarify".
    pub kind: String,
    pub entity_set: Option<String>,
    /// A second set to subtract, for a net figure (Invoices minus CreditNotes).
    pub subtract_entity_set: Option<String>,
    pub group_by: Vec<String>,
    pub metrics: Vec<MetricSpec>,
    pub filter: Option<FilterSpec>,
    pub time_window: Option<TimeWindow>,
    /// Present when kind is "clarify".
    pub clarifying_question: Option<String>,
    /// A one-line human description, shown as the "what I'll do" bubble. For
    /// kind "answer", this is the reply itself.
    pub summary: String,
}

impl Plan {
    pub fn parsed_kind(&self) -> PlanKind {
        match self.kind.as_str() {
            "aggregate" => PlanKind::Aggregate,
            "list" => PlanKind::List,
            "count" => PlanKind::Count,
            "answer" => PlanKind::Answer,
            _ => PlanKind::Clarify,
        }
    }

    pub fn is_clarify(&self) -> bool {
        self.parsed_kind() == PlanKind::Clarify
    }

    pub fn is_answer(&self) -> bool {
        self.parsed_kind() == PlanKind::Answer
    }

    /// Whether the plan is safe to run. Every field the model named must be in
    /// the report allowlist, and the metric/filter ops must be ones the server
    /// and the executor know. This is the hard guard, not the prompt.
    pub fn validated(&self) -> Result<(), String> {
        if self.is_clarify() || self.is_answer() {
            return Ok(());
        }
        let set = self
            .entity_set
            .as_deref()
            .ok_or("No entity set in the plan.")?;
        if query::report_fields(set).is_none() {
            return Err(format!("{set} is not a report entity set."));
        }
        if let Some(sub) = &self.subtract_entity_set {
            if query::report_fields(sub).is_none() {
                return Err(format!("{sub} is not a report entity set."));
            }
        }
        for m in &self.metrics {
            if !is_metric_op(&m.op) {
                return Err(format!("Unknown metric op {:?}.", m.op));
            }
            if m.op != "count" && !query::is_report_field(set, &m.field) {
                return Err(format!("{}.{} is not a report field.", set, m.field));
            }
            if let Some(sub) = &self.subtract_entity_set {
                if m.op != "count" && !query::is_report_field(sub, &m.field) {
                    return Err(format!("{}.{} is not a report field.", sub, m.field));
                }
            }
        }
        for d in &self.group_by {
            if !query::is_report_field(set, d) {
                return Err(format!("{}.{} is not a group-by field.", set, d));
            }
        }
        if let Some(f) = &self.filter {
            if !is_filter_op(&f.op) {
                return Err(format!("Unknown filter op {:?}.", f.op));
            }
            if !query::is_report_field(set, &f.field) {
                return Err(format!("{}.{} is not a filter field.", set, f.field));
            }
        }
        Ok(())
    }
}

fn is_metric_op(op: &str) -> bool {
    matches!(op, "sum" | "average" | "count")
}

fn is_filter_op(op: &str) -> bool {
    matches!(op, "ge" | "le" | "gt" | "lt" | "eq")
}

/// The slice of the catalogue the model sees. The full 460-set catalogue is
/// reference data and too large; only the report sets with verified fields, the
/// count-only sets and the relations are useful for a business question.
fn catalogue_context() -> String {
    let mut out = String::from("Report entity sets you may query (field lists are the only fields you may use):\n");
    for (set, fields) in query::REPORT_FIELDS {
        if fields.is_empty() {
            out.push_str(&format!("- {set}: (count only — use kind \"count\")\n"));
        } else {
            out.push_str(&format!("- {set}: {}\n", fields.join(", ")));
        }
    }
    out.push_str(
        "\nRelations you may $expand (foreign-key only):\n\
         - Orders/Invoices/CreditNotes/DeliveryNotes -> BusinessPartner (CardCode -> CardCode)\n\
         - PurchaseOrders/PurchaseInvoices/VendorPayments -> BusinessPartner\n\
         - IncomingPayments -> BusinessPartner\n\n\
         Query rules (do not violate):\n\
         - Never combine $filter with $apply; the server rejects it.\n\
         - Never use year()/month() in groupby; group by the raw DocDate field.\n\
         - Never order by an aggregate alias; sort in your plan, not in the query.\n\
         - Time windows are named symbols only: today, yesterday, this_week, last_week,\n\
           this_month, last_month, this_quarter, last_quarter, this_year, last_year,\n\
           last_30_days, all.\n\
         - Net sales = Invoices minus CreditNotes (subtractEntitySet \"CreditNotes\").\n\
           Net purchases = PurchaseInvoices minus PurchaseCreditNotes.\n",
    );
    out
}

const SYSTEM_PROMPT: &str = "You are Mochi, the business assistant connected to the user's SAP Business One. \
You talk to a business owner or a CEO in plain, warm, concise language — never jargon, never a raw field name \
like DocTotal or a query string. Welcome them first. Reply naturally to a greeting, a general question or a \
non-data question; only query the ERP when they ask about their own figures (sales, orders, customers, invoices, \
stock, receivables, purchases, employees, top customers, and so on).\
\n\n\
You may query the ERP. When you do, return a single JSON object and nothing else. No prose, no markdown fences.\
\n\n\
The JSON shape:\n\
{\n\
  \"kind\": \"aggregate\" | \"list\" | \"count\" | \"answer\" | \"clarify\",\n\
  \"entitySet\": \"Invoices\",\n\
  \"subtractEntitySet\": \"CreditNotes\" | null,\n\
  \"groupBy\": [\"DocDate\"],\n\
  \"metrics\": [{\"field\": \"DocTotal\", \"op\": \"sum\", \"alias\": \"Total\"}],\n\
  \"filter\": {\"field\": \"DocDate\", \"op\": \"ge\", \"value\": \"2025-01-01\"} | null,\n\
  \"timeWindow\": \"this_quarter\" | \"last_quarter\" | \"all\" | ... | null,\n\
  \"clarifyingQuestion\": null | \"Which period — this quarter, last quarter, or all time?\",\n\
  \"summary\": \"one line for the user, in plain language\"\n\
}\n\n\
Rules:\n\
- \"answer\" = no query. Put your conversational reply in summary. Use it for greetings,\n\
  thanks, general questions, or questions that are not about the user's ERP data.\n\
- If the question needs a time window or a choice and none is given, set kind to \"clarify\" and\n\
  put the question in clarifyingQuestion. Never guess a period.\n\
- Prefer timeWindow over filter for date ranges; only set filter for non-date conditions.\n\
- \"count\" uses the metrics array with op \"count\" and ignores the field; entitySet can be a\n\
  count-only set such as EmployeesInfo.\n\
- groupBy may be empty for a plain total, or one or more of the listed fields.\n\
- Keep the summary human: \"Sales for the last quarter, by month\" not \"groupby DocDate sum DocTotal\".\n";

/// The full system prompt for the planner.
fn prompt() -> String {
    format!("{SYSTEM_PROMPT}\n{}", catalogue_context())
}

/// Resolves the backend the planner should use: the SAP agent's binding, falling
/// back to the global chat backend. Returns an error when nothing is configured,
/// which the caller turns into the static-fallback path.
pub fn resolve_backend(settings: &Settings) -> Result<(String, String), String> {
    let (provider, model) = settings.binding_or_default("integration_sapb1");
    if provider.is_empty() {
        return Err("No chat backend configured for the ERP.".into());
    }
    Ok((provider, model))
}

/// Runs the planner: model → JSON → Plan → validation.
pub async fn plan(settings: &Settings, question: &str) -> Result<Plan, String> {
    let (provider_id, model) = resolve_backend(settings)?;
    let backend = providers::resolve(settings, &provider_id);
    if backend.needs_key() && backend.key.is_empty() {
        return Err(format!("{} API key missing.", backend.name));
    }
    let model = if model.trim().is_empty() {
        backend.default_model.clone()
    } else {
        model.trim().to_string()
    };
    if model.is_empty() {
        return Err("Pick a model for the SAP agent first.".into());
    }

    let raw = match backend.style {
        ApiStyle::Anthropic => {
            if backend.base().is_empty() {
                return Err("Set the endpoint URL for the SAP backend first.".into());
            }
            crate::claude::chat_plain(backend.base(), &model, &backend.key, &prompt(), question).await?
        }
        ApiStyle::OpenAICompatible => {
            if backend.base().is_empty() {
                return Err("Set the endpoint URL for the SAP backend first.".into());
            }
            let messages: Vec<serde_json::Value> = vec![
                serde_json::json!({ "role": "system", "content": prompt() }),
                serde_json::json!({ "role": "user", "content": question }),
            ];
            crate::openai::chat(backend.base(), Some(backend.key.as_str()), &model, &messages).await?
        }
    };

    let plan: Plan = parse_plan_json(&raw)?;
    plan.validated()?;
    Ok(plan)
}

/// Extracts the first JSON object from the model's answer. Models sometimes wrap
/// the object in prose or a fence despite the instruction, so this is tolerant
/// while still requiring a real object.
fn parse_plan_json(raw: &str) -> Result<Plan, String> {
    let start = raw.find('{').ok_or("The model returned no JSON.")?;
    let end = raw.rfind('}').ok_or("The model returned no JSON.")?;
    let slice = &raw[start..=end];
    let plan: Plan =
        serde_json::from_str(slice).map_err(|e| format!("Bad plan from the model: {e}"))?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_plan() -> Plan {
        Plan {
            kind: "aggregate".into(),
            entity_set: Some("Invoices".into()),
            subtract_entity_set: None,
            group_by: vec!["DocDate".into()],
            metrics: vec![MetricSpec {
                field: "DocTotal".into(),
                op: "sum".into(),
                alias: "Total".into(),
            }],
            filter: None,
            time_window: Some(TimeWindow::LastQuarter),
            clarifying_question: None,
            summary: "Sum Invoices by month for last quarter.".into(),
        }
    }

    #[test]
    fn a_ready_plan_validates() {
        assert!(valid_plan().validated().is_ok());
    }

    #[test]
    fn an_unknown_entity_set_is_rejected() {
        let mut p = valid_plan();
        p.entity_set = Some("AccountSegmentationCategories".into());
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_field_outside_the_allowlist_is_rejected() {
        let mut p = valid_plan();
        p.metrics[0].field = "DocumentLines".into();
        assert!(p.validated().is_err());
    }

    #[test]
    fn an_unknown_metric_op_is_rejected() {
        let mut p = valid_plan();
        p.metrics[0].op = "multiply".into();
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_count_metric_ignores_its_field() {
        let mut p = valid_plan();
        p.metrics = vec![MetricSpec {
            field: String::new(),
            op: "count".into(),
            alias: "Cnt".into(),
        }];
        assert!(p.validated().is_ok());
    }

    #[test]
    fn an_unknown_filter_op_is_rejected() {
        let mut p = valid_plan();
        p.filter = Some(FilterSpec {
            field: "DocDate".into(),
            op: "like".into(),
            value: "x".into(),
        });
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_filter_on_a_non_report_field_is_rejected() {
        let mut p = valid_plan();
        p.filter = Some(FilterSpec {
            field: "U_MyField".into(),
            op: "eq".into(),
            value: "x".into(),
        });
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_clarify_plan_skips_validation() {
        let p = Plan {
            kind: "clarify".into(),
            entity_set: None,
            subtract_entity_set: None,
            group_by: vec![],
            metrics: vec![],
            filter: None,
            time_window: None,
            clarifying_question: Some("Which period?".into()),
            summary: "Need a period.".into(),
        };
        assert!(p.validated().is_ok());
        assert!(p.is_clarify());
    }

    #[test]
    fn json_is_parsed_from_inside_prose() {
        let raw = "Here you go:\n```json\n{\"kind\":\"aggregate\",\"entitySet\":\"Invoices\",\
            \"subtractEntitySet\":null,\"groupBy\":[\"DocDate\"],\"metrics\":[{\"field\":\"DocTotal\",\"op\":\"sum\",\"alias\":\"Total\"}],\
            \"filter\":null,\"timeWindow\":\"this_quarter\",\"clarifyingQuestion\":null,\
            \"summary\":\"Sum by month.\"}\n```";
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.entity_set.as_deref(), Some("Invoices"));
        assert_eq!(p.parsed_kind(), PlanKind::Aggregate);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_plan_json("no json here").is_err());
        assert!(parse_plan_json("{\"kind\":\"aggregate\"}").is_ok(), "unknown fields are allowed");
    }

    #[test]
    fn the_catalogue_context_names_the_three_key_sets() {
        let ctx = catalogue_context();
        assert!(ctx.contains("Invoices"));
        assert!(ctx.contains("BusinessPartners"));
        assert!(ctx.contains("PurchaseInvoices"));
    }

    #[test]
    fn every_time_window_name_is_known() {
        for name in ["today", "yesterday", "this_week", "last_week", "this_month", "last_month",
            "this_quarter", "last_quarter", "this_year", "last_year", "last_30_days", "all"] {
            assert!(
                serde_json::from_str::<TimeWindow>(&format!("\"{name}\"")).is_ok(),
                "{name} is not a known time window"
            );
        }
    }
}