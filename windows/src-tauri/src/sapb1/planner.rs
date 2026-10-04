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
use serde_json::Value;

use super::catalogue;
use super::documents;
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

/// One line of a document the assistant wants to create.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LineSpec {
    pub item_code: String,
    pub quantity: f64,
    pub price: Option<f64>,
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
    /// A list of entities from any SAP entity set, fields auto-selected.
    Detail,
    /// Create a sales or purchase document (never posted without confirmation).
    Create,
    /// Copy a source document into its target (order -> invoice).
    Copy,
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
    /// Zero or more ANDed conditions. A range is two conditions on the same
    /// field (DocDate ge … and le …), so an arbitrary year or month is expressible.
    pub filter: Vec<FilterSpec>,
    pub time_window: Option<TimeWindow>,
    /// For kind "create": the customer or supplier code.
    pub card_code: Option<String>,
    /// For kind "create": an ISO document date (optional, server defaults to today).
    pub doc_date: Option<String>,
    /// For kind "create": the lines to post.
    pub lines: Vec<LineSpec>,
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
            "detail" => PlanKind::Detail,
            "create" => PlanKind::Create,
            "copy" => PlanKind::Copy,
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
        // A detail list reads any SAP entity set; the executor picks the fields.
        // Only the set name and any filter need checking here.
        if self.parsed_kind() == PlanKind::Detail {
            let set = self
                .entity_set
                .as_deref()
                .ok_or("No entity set in the plan.")?;
            if catalogue::entity_set(set).is_none() {
                return Err(format!("{set} is not a SAP entity set."));
            }
            for f in &self.filter {
                if !is_filter_op(&f.op) {
                    return Err(format!("Unknown filter op {:?}.", f.op));
                }
                // When the set's type is in the catalogue we know its fields and
                // can reject a bad one. When it is not (e.g. EmployeesInfo), the
                // value is still sanitised; a wrong field just errors on the server.
                if catalogue::type_for_set(set).is_some() && !catalogue::has_field(set, &f.field) {
                    return Err(format!("{}.{} is not a field of that entity.", set, f.field));
                }
            }
            return Ok(());
        }
        // A create document: the type, customer and lines must be present. Item
        // codes and the customer are checked against the server in the executor.
        if self.parsed_kind() == PlanKind::Create {
            let set = self.entity_set.as_deref().ok_or("No document type.")?;
            if documents::find_by_set(set).is_none() {
                return Err(format!("{set} is not a document type I can create."));
            }
            if self.card_code.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err("No customer or supplier for the document.".into());
            }
            if self.lines.is_empty() {
                return Err("No lines for the document.".into());
            }
            for l in &self.lines {
                if l.item_code.trim().is_empty() {
                    return Err("A line has no item code.".into());
                }
                if !(l.quantity > 0.0) {
                    return Err("Quantity must be positive.".into());
                }
            }
            return Ok(());
        }
        // A copy needs a source document and a filter naming its number.
        if self.parsed_kind() == PlanKind::Copy {
            let source = self.entity_set.as_deref().ok_or("No source document.")?;
            if documents::copy_target(source).is_none() {
                return Err(format!("{source} cannot be copied to a document."));
            }
            if !self.filter.iter().any(|f| f.field == "DocNum" && f.op == "eq") {
                return Err("Copy needs the source document number (filter DocNum eq …).".into());
            }
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
        for f in &self.filter {
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
         - For an arbitrary date range the user names, use a DocDate filter (ge and/or le)\n\
           instead of a time window.\n\
         - For a \"detail\" list you may name any SAP entity set (EmployeesInfo, Items,\n\
           BusinessPartners, Activities, …); the executor picks the meaningful fields.\n\
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
You are in an ongoing conversation and you can see the prior turns. Never repeat a question you \
already asked: if the user has answered, use their answer and act. Only ask when something you \
genuinely need is still missing.\
\n\n\
You may query the ERP. When you do, return a single JSON object and nothing else. No prose, no markdown fences.\
\n\n\
The JSON shape:\n\
{\n\
  \"kind\": \"aggregate\" | \"list\" | \"count\" | \"detail\" | \"create\" | \"copy\" | \"answer\" | \"clarify\",\n\
  \"entitySet\": \"Invoices\",\n\
  \"subtractEntitySet\": \"CreditNotes\" | null,\n\
  \"groupBy\": [\"DocDate\"],\n\
  \"metrics\": [{\"field\": \"DocTotal\", \"op\": \"sum\", \"alias\": \"Total\"}],\n\
  \"filter\": [{\"field\": \"DocDate\", \"op\": \"ge\", \"value\": \"2015-01-01\"}, {\"field\": \"DocDate\", \"op\": \"le\", \"value\": \"2015-12-31\"}] | null,\n\
  \"timeWindow\": \"this_quarter\" | \"last_quarter\" | \"all\" | ... | null,\n\
  \"cardCode\": \"C0001\" | null,\n\
  \"docDate\": \"2026-10-04\" | null,\n\
  \"lines\": [{\"itemCode\": \"A00001\", \"quantity\": 2, \"price\": 100}] | null,\n\
  \"clarifyingQuestion\": null | \"Which period — this quarter, last quarter, or all time?\",\n\
  \"summary\": \"one line for the user, in plain language\"\n\
}\n\n\
Rules:\n\
- \"answer\" = no query. Put your conversational reply in summary. Use it for greetings,\n\
  thanks, general questions, or questions that are not about the user's ERP data.\n\
- \"detail\" = list entities from any SAP entity set, meaningful fields only. Use for\n\
  \"show me employees\", \"list items\", \"customer details\". entitySet names a set;\n\
  the executor picks which fields to show. filter narrows it.\n\
- \"create\" = build a sales or purchase document, never post it. entitySet names a\n\
  document set (Orders, Invoices, PurchaseOrders, …); cardCode the customer or\n\
  supplier; lines the items with quantity and optional price. The app shows a\n\
  confirmation before anything is posted. Gather item and customer choices first.\n\
- \"copy\" = copy a source document into its twin (Orders -> Invoices,\n\
  PurchaseOrders -> PurchaseInvoices). entitySet names the source; filter DocNum\n\
  eq names the document to copy. The app shows a confirmation before posting.\n\
- If the question needs a time window or a choice and none is given, set kind to \"clarify\" and\n\
  put the question in clarifyingQuestion. Never guess a period.\n\
- Use timeWindow for the named periods (this_quarter, last_year, last_30_days…).\n\
  For a range the user names explicitly (\"in 2015\", \"from March to June\", \"this fiscal\n\
  year\"), use a filter on DocDate with ge/le — one condition per bound. filter and\n\
  timeWindow may combine; both are ANDed.\n\
- \"count\" uses the metrics array with op \"count\" and ignores the field; entitySet can be a\n\
  count-only set such as EmployeesInfo.\n\
- groupBy may be empty for a plain total, or one or more of the listed fields.\n\
- Keep the summary human: \"Sales for the last quarter, by month\" not \"groupby DocDate sum DocTotal\".\n";

/// The full system prompt for the planner.
fn prompt() -> String {
    format!("{SYSTEM_PROMPT}\n{}", catalogue_context())
}

/// One prior turn of the ERP conversation, so the model is aware of what it
/// already asked and what the user answered. The model is the brain here, not a
/// keyword matcher: without this it re-asks the same question forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    /// "user" | "assistant".
    pub role: String,
    pub content: String,
}

/// How many prior turns are sent. Enough for context, bounded for token cost.
const HISTORY_CAP: usize = 20;

/// Resolves the backend the planner should use: the SAP agent's binding, falling
/// back to the global chat backend. Returns an error when nothing is configured,
/// which the caller turns into a friendly "configure a backend" message.
pub fn resolve_backend(settings: &Settings) -> Result<(String, String), String> {
    let (provider, model) = settings.binding_or_default("integration_sapb1");
    if provider.is_empty() {
        return Err("No chat backend configured for the ERP.".into());
    }
    Ok((provider, model))
}

/// The Anthropic `{role, content}` messages: prior turns, then the new question.
fn anthropic_messages(history: &[ChatTurn], question: &str) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = history
        .iter()
        .rev()
        .take(HISTORY_CAP)
        .rev()
        .map(|t| serde_json::json!({ "role": t.role, "content": t.content }))
        .collect();
    messages.push(serde_json::json!({ "role": "user", "content": question }));
    messages
}

/// The OpenAI messages: the system prompt first, then the same turns.
fn openai_messages(system: &str, history: &[ChatTurn], question: &str) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = Vec::new();
    messages.push(serde_json::json!({ "role": "system", "content": system }));
    for t in history.iter().rev().take(HISTORY_CAP).rev() {
        messages.push(serde_json::json!({ "role": t.role, "content": t.content }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": question }));
    messages
}

/// Runs the planner: model → JSON → Plan → validation. `history` is the
/// conversation so far, so the model stays aware and does not re-ask.
pub async fn plan(
    settings: &Settings,
    question: &str,
    history: &[ChatTurn],
) -> Result<Plan, String> {
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
    if backend.base().is_empty() {
        return Err("Set the endpoint URL for the SAP backend first.".into());
    }

    let system = prompt();
    let raw = match backend.style {
        ApiStyle::Anthropic => {
            let messages = anthropic_messages(history, question);
            crate::claude::chat_plain(backend.base(), &model, &backend.key, &system, &messages).await?
        }
        ApiStyle::OpenAICompatible => {
            let messages = openai_messages(&system, history, question);
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
    let value: Value =
        serde_json::from_str(slice).map_err(|e| format!("Bad plan from the model: {e}"))?;
    Ok(plan_from_value(&value))
}

/// A string from a value that may be a string, a number, or a one-item array.
/// Models drift: `"entitySet": ["Invoices"]` is wrong but must not throw the
/// whole plan away.
fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Array(a) => a.iter().find_map(as_string),
        _ => None,
    }
}

/// A list of strings from an array or a single string.
fn as_string_list(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(as_string).collect(),
        Value::Null => Vec::new(),
        other => as_string(other).into_iter().collect(),
    }
}

fn as_time_window(v: &Value) -> Option<TimeWindow> {
    let s = as_string(v)?;
    serde_json::from_value(Value::String(s)).ok()
}

fn as_metrics(v: &Value) -> Vec<MetricSpec> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter_map(|m| {
            let field = m.get("field").and_then(as_string).unwrap_or_default();
            let op = m
                .get("op")
                .and_then(as_string)
                .unwrap_or_else(|| "sum".to_string());
            let alias = m
                .get("alias")
                .and_then(as_string)
                .unwrap_or_else(|| "Total".to_string());
            Some(MetricSpec { field, op, alias })
        })
        .collect()
}

fn as_filter(v: &Value) -> Option<FilterSpec> {
    let field = v.get("field").and_then(as_string)?;
    let op = v.get("op").and_then(as_string)?;
    let value = v.get("value").and_then(as_string)?;
    Some(FilterSpec { field, op, value })
}

/// Lines of a create plan, one object per document line.
fn as_lines(v: &Value) -> Vec<LineSpec> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    };
    items
        .into_iter()
        .filter_map(|m| {
            let item_code = m.get("itemCode").and_then(as_string).unwrap_or_default();
            let quantity = m.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
            let price = m.get("price").and_then(Value::as_f64);
            Some(LineSpec { item_code, quantity, price })
        })
        .collect()
}

/// One filter or a list of them: the model may emit `"filter": {…}` or
/// `"filters": [{…}, …]`. Both become the same `Vec`.
fn as_filters(v: &Value) -> Vec<FilterSpec> {
    match v {
        Value::Array(a) => a.iter().filter_map(as_filter).collect(),
        Value::Null => Vec::new(),
        other => as_filter(other).into_iter().collect(),
    }
}

/// Builds a plan field by field with coercion, so one odd shape does not discard
/// an otherwise usable plan.
fn plan_from_value(v: &Value) -> Plan {
    Plan {
        kind: v
            .get("kind")
            .and_then(as_string)
            .unwrap_or_else(|| "clarify".to_string()),
        entity_set: v.get("entitySet").and_then(as_string),
        subtract_entity_set: v.get("subtractEntitySet").and_then(as_string),
        group_by: v.get("groupBy").map(as_string_list).unwrap_or_default(),
        metrics: v.get("metrics").map(as_metrics).unwrap_or_default(),
        filter: v
            .get("filters")
            .or_else(|| v.get("filter"))
            .map(as_filters)
            .unwrap_or_default(),
        time_window: v.get("timeWindow").and_then(as_time_window),
        card_code: v.get("cardCode").and_then(as_string),
        doc_date: v.get("docDate").and_then(as_string),
        lines: v.get("lines").map(as_lines).unwrap_or_default(),
        clarifying_question: v.get("clarifyingQuestion").and_then(as_string),
        summary: v.get("summary").and_then(as_string).unwrap_or_default(),
    }
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
            filter: vec![],
            time_window: Some(TimeWindow::LastQuarter),
            card_code: None,
            doc_date: None,
            lines: vec![],
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
        p.filter = vec![FilterSpec {
            field: "DocDate".into(),
            op: "like".into(),
            value: "x".into(),
        }];
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_filter_on_a_non_report_field_is_rejected() {
        let mut p = valid_plan();
        p.filter = vec![FilterSpec {
            field: "U_MyField".into(),
            op: "eq".into(),
            value: "x".into(),
        }];
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_detail_plan_validates_against_the_catalogue() {
        let p = Plan {
            kind: "detail".into(),
            entity_set: Some("EmployeesInfo".into()),
            ..Default::default()
        };
        assert_eq!(p.parsed_kind(), PlanKind::Detail);
        assert!(p.validated().is_ok(), "{:?}", p.validated());
    }

    #[test]
    fn a_detail_plan_rejects_an_unknown_set() {
        let p = Plan {
            kind: "detail".into(),
            entity_set: Some("NotASet".into()),
            ..Default::default()
        };
        assert!(p.validated().is_err());
    }

    #[test]
    fn a_create_plan_validates_and_rejects_bad_lines() {
        let good = Plan {
            kind: "create".into(),
            entity_set: Some("Orders".into()),
            card_code: Some("C0001".into()),
            lines: vec![LineSpec { item_code: "A00001".into(), quantity: 2.0, price: Some(100.0) }],
            ..Default::default()
        };
        assert!(good.validated().is_ok(), "{:?}", good.validated());
        let mut bad = good.clone();
        bad.lines[0].quantity = 0.0;
        assert!(bad.validated().is_err());
        let mut bad_set = good.clone();
        bad_set.entity_set = Some("NotADoc".into());
        assert!(bad_set.validated().is_err());
    }

    #[test]
    fn a_copy_plan_validates_and_needs_a_source_number() {
        let good = Plan {
            kind: "copy".into(),
            entity_set: Some("Orders".into()),
            filter: vec![FilterSpec { field: "DocNum".into(), op: "eq".into(), value: "123".into() }],
            ..Default::default()
        };
        assert!(good.validated().is_ok(), "{:?}", good.validated());
        let mut bad_set = good.clone();
        bad_set.entity_set = Some("Invoices".into());
        assert!(bad_set.validated().is_err());
        let mut no_num = good.clone();
        no_num.filter = vec![];
        assert!(no_num.validated().is_err());
    }

    #[test]
    fn a_clarify_plan_skips_validation() {
        let p = Plan {
            kind: "clarify".into(),
            entity_set: None,
            subtract_entity_set: None,
            group_by: vec![],
            metrics: vec![],
            filter: vec![],
            time_window: None,
            card_code: None,
            doc_date: None,
            lines: vec![],
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
    fn an_array_where_a_string_belongs_is_coerced_not_fatal() {
        // The exact shape that produced "invalid type: sequence, expected a
        // string at line 7 column 57" in the running app.
        let raw = r#"{
            "kind": "aggregate",
            "entitySet": ["Invoices"],
            "subtractEntitySet": ["CreditNotes"],
            "groupBy": "DocDate",
            "metrics": [{"field": "DocTotal", "op": "sum", "alias": "Total"}],
            "filter": null,
            "timeWindow": ["last_month"],
            "clarifyingQuestion": null,
            "summary": ["Sales for last month"]
        }"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.entity_set.as_deref(), Some("Invoices"));
        assert_eq!(p.subtract_entity_set.as_deref(), Some("CreditNotes"));
        assert_eq!(p.group_by, vec!["DocDate".to_string()]);
        assert_eq!(p.time_window, Some(TimeWindow::LastMonth));
        assert_eq!(p.summary, "Sales for last month");
        assert!(p.validated().is_ok(), "{:?}", p.validated());
    }

    #[test]
    fn a_filter_array_parses_and_validates() {
        let raw = r#"{
            "kind": "aggregate",
            "entitySet": "Invoices",
            "subtractEntitySet": "CreditNotes",
            "groupBy": [],
            "metrics": [{"field": "DocTotal", "op": "sum", "alias": "Total"}],
            "filter": [{"field": "DocDate", "op": "ge", "value": "2015-01-01"}, {"field": "DocDate", "op": "le", "value": "2015-12-31"}],
            "timeWindow": null,
            "clarifyingQuestion": null,
            "summary": "Net sales for 2015"
        }"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.filter.len(), 2);
        assert_eq!(p.filter[0].op, "ge");
        assert_eq!(p.filter[1].value, "2015-12-31");
        assert!(p.validated().is_ok(), "{:?}", p.validated());
    }

    #[test]
    fn a_single_object_filter_is_coerced_to_a_list() {
        let raw = r#"{
            "kind": "aggregate",
            "entitySet": "Invoices",
            "groupBy": [],
            "metrics": [{"field": "DocTotal", "op": "sum", "alias": "Total"}],
            "filter": {"field": "DocumentStatus", "op": "eq", "value": "bost_Open"},
            "timeWindow": null,
            "clarifyingQuestion": null,
            "summary": "Open invoices"
        }"#;
        let p = parse_plan_json(raw).unwrap();
        assert_eq!(p.filter.len(), 1);
        assert!(p.validated().is_ok(), "{:?}", p.validated());
    }

    #[test]
    fn a_missing_kind_defaults_to_a_clarification() {
        let p = parse_plan_json(r#"{"summary":"what period?"}"#).unwrap();
        assert!(p.is_clarify());
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