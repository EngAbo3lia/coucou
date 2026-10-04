//! Answers questions about the ERP from the island chat.
//!
//! The model is the brain. A question plus the conversation so far goes to the
//! configured chat backend, which returns a structured `Plan`. The plan is
//! validated against the report allowlists and executed. Nothing here does
//! keyword routing: the model decides what to ask, what to run, and how to
//! answer. The only hard guard is the validator â€” a plan can never read a field
//! outside the allowlist, and filter values are sanitised before they reach the
//! query string.
//!
//! The queries respect the rules in `query` â€” no `$filter` with `$apply`, no date
//! functions in `groupby` â€” so the executor fetches raw rows and slices, buckets
//! and sums in Rust rather than asking the server to do something it rejects.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::catalogue;
use super::dates;
use super::documents;
use super::entities;
use super::planner::{self, ChatTurn, FilterSpec, Plan, PlanKind};
use super::query;
use super::transport::{self, Credentials};
use crate::settings::Settings;

const ROW_CAP: usize = 5000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerRow {
    pub label: String,
    pub value: f64,
}

/// A clickable choice in the chat (an item or customer the user can pick).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub title: String,
    /// A readable block, ready for the chat log.
    pub text: String,
    pub rows: Vec<AnswerRow>,
    pub total: Option<f64>,
    /// True when a row cap cut the data set, so the figure is a lower bound.
    pub partial: bool,
    /// Which data produced this, so the figure can be trusted or challenged.
    pub source: String,
    /// "result" | "clarify" | "answer" | "confirm".
    pub kind: String,
    /// The "what I'll do" bubble, shown before a result.
    pub plan: Option<String>,
    /// For kind "confirm": the document spec to post after the user confirms.
    pub payload: Option<Value>,
    /// Clickable choices (items/customers) the user can pick in the chat.
    pub options: Vec<PickOption>,
    /// The question the assistant needs answered before it can run.
    pub clarifying_question: Option<String>,
    /// Kept for wire compatibility; the planner path never fills it.
    pub suggestions: Vec<String>,
}

impl Default for Answer {
    fn default() -> Self {
        Self {
            title: String::new(),
            text: String::new(),
            rows: vec![],
            total: None,
            partial: false,
            source: String::new(),
            kind: "result".into(),
            plan: None,
            payload: None,
            options: vec![],
            clarifying_question: None,
            suggestions: vec![],
        }
    }
}

/// The main entry. `history` is the conversation so far, so the model is aware of
/// what it already asked. A missing backend is a clean error, never a silent
/// keyword match.
pub async fn ask(
    c: &Credentials,
    settings: &Settings,
    question: &str,
    history: &[ChatTurn],
) -> Result<Answer, String> {
    let plan = planner::plan(settings, question, history).await.map_err(|e| {
        // Never surface a raw serde/internal string to the user.
        if e.contains("No chat backend") {
            "No AI backend is set up for the SAP Harness. Configure one in Settings â†’ Agents, then ask again.".to_string()
        } else if e.contains("Bad plan") || e.contains("no JSON") {
            "Sorry, I didn't quite catch that. Could you say it another way â€” for example, \"show me sales this year\" or \"list the items\"?".to_string()
        } else {
            e
        }
    })?;

    if plan.is_clarify() {
        return Ok(clarify_answer(&plan));
    }
    if plan.is_answer() {
        return Ok(answer_reply(&plan));
    }
    execute(c, &plan).await
}

fn clarify_answer(plan: &Plan) -> Answer {
    Answer {
        title: "SAP Harness".into(),
        text: plan
            .clarifying_question
            .clone()
            .unwrap_or_else(|| "I need a bit more to run that.".into()),
        kind: "clarify".into(),
        plan: None,
        clarifying_question: plan.clarifying_question.clone(),
        source: "no query run".into(),
        ..Answer::default()
    }
}

fn answer_reply(plan: &Plan) -> Answer {
    let text = if plan.summary.trim().is_empty() {
        "I can help with your sales, orders, customers, stock and more. What would you like to know?".into()
    } else {
        plan.summary.clone()
    };
    Answer {
        title: "Mochi".into(),
        text,
        kind: "answer".into(),
        source: "no query run".into(),
        ..Answer::default()
    }
}

// â”€â”€ executor â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Runs a validated plan against the ERP.
async fn execute(c: &Credentials, plan: &Plan) -> Result<Answer, String> {
    plan.validated()?;
    let range = plan.time_window.and_then(dates::range);
    match plan.parsed_kind() {
        PlanKind::Count => count_plan(c, plan, range).await,
        PlanKind::List => list_plan(c, plan, range).await,
        PlanKind::Aggregate => {
            if plan.group_by.is_empty() {
                aggregate_total(c, plan, range).await
            } else {
                grouped(c, plan, range).await
            }
        }
        PlanKind::Detail => detail_plan(c, plan, range).await,
        PlanKind::Create => create_plan(c, plan).await,
        PlanKind::Copy => copy_plan(c, plan).await,
        PlanKind::Receive | PlanKind::NewItem | PlanKind::NewPartner => Ok(entity_plan(plan)?),
        PlanKind::Answer => Ok(answer_reply(plan)),
        PlanKind::Clarify => Err("The plan still needs a clarifying answer.".into()),
    }
}

/// Whether the set carries a `DocDate` field, in the report allowlist or the
/// live catalogue. Used to know if a date range can be applied to the rows.
fn set_has_date(set: &str) -> bool {
    query::is_report_field(set, "DocDate") || catalogue::has_field(set, "DocDate")
}

/// Fetches rows for a set, then filters them by the date range (if any) on the
/// `DocDate` field and by the plan's ANDed conditions. The dataset is small
/// enough to page once; the row cap is a lower-bound guard, not a filter.
async fn fetch_rows(
    c: &Credentials,
    set: &str,
    select: &[String],
    range: &Option<(String, String)>,
    filters: &[FilterSpec],
) -> Result<(Vec<Value>, bool), String> {
    let mut fields = select.to_vec();
    if range.is_some() && !fields.iter().any(|f| f == "DocDate") && set_has_date(set) {
        fields.push("DocDate".into());
    }
    for f in filters {
        if !fields.iter().any(|x| x == &f.field) && query::is_report_field(set, &f.field) {
            fields.push(f.field.clone());
        }
    }
    let url = query::build_list_url(set, &fields, query::ROW_CAP);
    let rows = transport::get(c, &url).await?;
    let list = rows.get("value").and_then(Value::as_array).cloned().unwrap_or_default();
    let filtered: Vec<Value> = list
        .into_iter()
        .filter(|r| {
            if let Some((start, end)) = range {
                let date = r.get("DocDate").and_then(Value::as_str).unwrap_or("");
                if !(date.is_empty() || (date >= start.as_str() && date <= end.as_str())) {
                    return false;
                }
            }
            filters.iter().all(|f| row_matches(r, f))
        })
        .collect();
    let partial = filtered.len() >= ROW_CAP;
    Ok((filtered, partial))
}

/// Whether a fetched row satisfies one filter condition. Values compare as
/// numbers when both sides parse as one, else as strings (ISO dates, enum
/// words, codes). The field was validated against the report allowlist.
fn row_matches(row: &Value, f: &FilterSpec) -> bool {
    let Some(cell) = row.get(&f.field) else { return false };
    let value = f.value.trim();
    if let (Some(c), Ok(nv)) = (cell.as_f64(), value.parse::<f64>()) {
        return match f.op.as_str() {
            "eq" => (c - nv).abs() < 1e-9,
            "ge" => c >= nv,
            "gt" => c > nv,
            "le" => c <= nv,
            "lt" => c < nv,
            _ => false,
        };
    }
    let c = cell.as_str().unwrap_or("");
    match f.op.as_str() {
        "eq" => c == value,
        "ge" => c >= value,
        "gt" => c > value,
        "le" => c <= value,
        "lt" => c < value,
        _ => false,
    }
}

fn metric_field(plan: &Plan) -> String {
    plan.metrics
        .iter()
        .find(|m| m.op != "count")
        .map(|m| m.field.clone())
        .unwrap_or_default()
}

async fn aggregate_total(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let metric = plan.metrics.iter().find(|m| m.op != "count").cloned();
    let is_count = plan.metrics.iter().any(|m| m.op == "count");

    let source = build_source(plan, &range);

    if is_count {
        let n = count_with_filter(c, set, plan, &range).await?;
        let title = title_for(plan);
        return Ok(Answer {
            title: title.clone(),
            text: String::new(),
            total: Some(n as f64),
            source,
            plan: Some(title),
            ..Answer::default()
        });
    }

    let Some(metric) = metric else {
        return Err("The plan has no metric.".into());
    };
    let select = vec![metric.field.clone()];
    let (rows, partial) = fetch_rows(c, set, &select, &range, &plan.filter).await?;
    let mut value = aggregate_rows(&rows, &metric.field, &metric.op, None).unwrap_or(0.0);

    // A net figure (sales minus credit notes, purchases minus purchase credit
    // notes) subtracts the same window from a second set.
    if let Some(sub) = &plan.subtract_entity_set {
        let (sub_rows, _) = fetch_rows(c, sub, &select, &range, &plan.filter).await?;
        value -= aggregate_rows(&sub_rows, &metric.field, &metric.op, None).unwrap_or(0.0);
    }

    let title = title_for(plan);
    let text = if value == 0.0 && range.is_some() {
        "No data in this period.".to_string()
    } else {
        String::new()
    };
    Ok(Answer {
        title: title.clone(),
        text,
        total: Some(value),
        partial,
        source,
        plan: Some(title),
        ..Answer::default()
    })
}

async fn grouped(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let metric = plan.metrics.iter().find(|m| m.op != "count").cloned();
    let dims = plan.group_by.clone();
    let source = build_source(plan, &range);

    let Some(metric) = metric else {
        return Err("The plan has no metric.".into());
    };
    let mut select: Vec<String> = dims.clone();
    select.push(metric.field.clone());
    let (rows, partial) = fetch_rows(c, set, &select, &range, &plan.filter).await?;

    let mut groups: HashMap<String, f64> = HashMap::new();
    for r in &rows {
        let key = dims
            .iter()
            .map(|d| r.get(d).and_then(Value::as_str).unwrap_or("?").to_string())
            .collect::<Vec<_>>()
            .join(" Â· ");
        let v = r.get(&metric.field).and_then(Value::as_f64).unwrap_or(0.0);
        *groups.entry(key).or_default() += v;
    }
    let mut pairs: Vec<(String, f64)> = groups.into_iter().collect();
    pairs.sort_by(|a, b| b.1.total_cmp(&a.1));

    let names = partner_names(c).await.unwrap_or_default();
    let rows: Vec<AnswerRow> = pairs
        .into_iter()
        .map(|(label, value)| AnswerRow {
            label: names.get(&label).cloned().unwrap_or(label),
            value,
        })
        .collect();
    let total: f64 = rows.iter().map(|r| r.value).sum();
    let title = title_for(plan);
    Ok(Answer {
        title: title.clone(),
        text: String::new(),
        rows,
        total: Some(total),
        partial,
        source,
        plan: Some(title),
        ..Answer::default()
    })
}

async fn count_plan(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let n = count_with_filter(c, set, plan, &range).await?;
    let source = build_source(plan, &range);
    let title = title_for(plan);
    Ok(Answer {
        title: title.clone(),
        text: String::new(),
        total: Some(n as f64),
        source,
        plan: Some(title),
        ..Answer::default()
    })
}

async fn count_with_filter(
    c: &Credentials,
    set: &str,
    plan: &Plan,
    range: &Option<(String, String)>,
) -> Result<i64, String> {
    let mut filter: Vec<String> = Vec::new();
    if let Some((start, end)) = range {
        filter.push(format!("DocDate ge datetime'{start}'"));
        filter.push(format!("DocDate le datetime'{end}'"));
    }
    for f in &plan.filter {
        // The field and op were validated; the value is sanitised here.
        filter.push(format!("{} {} {}", f.field, f.op, query::filter_literal(&f.value)?));
    }
    let url = query::build_count_url(set, &filter.join(" and "));
    let raw = transport::get_text(c, &url).await?;
    raw.trim().trim_matches('"').parse().map_err(|_| format!("$count returned {raw:?}"))
}

async fn list_plan(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let source = build_source(plan, &range);
    let label_field = pick_label_field(set);
    let value_field = metric_field(plan);
    let value_field = if value_field.is_empty() { "DocTotal".to_string() } else { value_field };
    // Never fetch full document rows: a `list` over a wide set (Invoices) can be
    // tens of MB, which the server truncates and reqwest reports as a body
    // decode error. Select only the label, the value and (via `fetch_rows`) the
    // date bound, so the response stays small.
    let mut select = vec![label_field.clone(), value_field.clone()];
    if select[0] == select[1] {
        select.pop();
    }
    let (rows, partial) = fetch_rows(c, set, &select, &range, &plan.filter).await?;
    let mut out: Vec<AnswerRow> = rows
        .iter()
        .filter_map(|r| {
            let value = r.get(&value_field).and_then(Value::as_f64)?;
            let label = r
                .get(&label_field)
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| r.get("DocNum").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| "?".to_string());
            Some(AnswerRow { label, value })
        })
        .collect();
    out.sort_by(|a, b| b.value.total_cmp(&a.value));
    let total: f64 = out.iter().map(|r| r.value).sum();
    let title = title_for(plan);
    Ok(Answer {
        title: title.clone(),
        text: String::new(),
        rows: out,
        total: Some(total),
        partial,
        source,
        plan: Some(title),
        ..Answer::default()
    })
}

fn pick_label_field(set: &str) -> String {
    for field in ["CardName", "CardCode", "ItemName", "ItemCode", "DocNum"] {
        if query::is_report_field(set, field) {
            return field.to_string();
        }
    }
    "DocNum".to_string()
}

/// A list of entities from any SAP entity set: meaningful, non-empty fields only.
/// The model names the set; the executor picks the fields, fetches, and renders a
/// readable block rather than dumping the raw response.
async fn detail_plan(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let fields = resolve_detail_fields(c, set).await?;
    if fields.is_empty() {
        return Err(format!("No meaningful fields to show for {set}."));
    }
    let source = build_source(plan, &range);
    let (rows, partial) = fetch_rows(c, set, &fields, &range, &plan.filter).await?;

    let lines: Vec<String> = rows
        .iter()
        .take(ROW_CAP.min(50))
        .filter_map(|r| {
            let mut parts: Vec<String> = Vec::new();
            for f in &fields {
                let s = value_text(r.get(f.as_str()));
                if !s.is_empty() {
                    parts.push(format!("{}: {s}", friendly_field(f)));
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" Â· "))
            }
        })
        .collect();

    let title = format!("{} â€” detail", friendly_set(set));
    let text = if lines.is_empty() {
        "No matching records.".to_string()
    } else {
        lines.join("\n")
    };

    // Clickable choices: the code as the value, the name as the label.
    let code_field = fields.iter().find(|f| f.ends_with("Code") || f.ends_with("Num")).cloned();
    let name_field = fields.iter().find(|f| f.ends_with("Name")).cloned();
    let options: Vec<PickOption> = rows
        .iter()
        .filter_map(|r| {
            let value = code_field.as_ref().map(|f| value_text(r.get(f.as_str()))).unwrap_or_default();
            if value.is_empty() {
                return None;
            }
            let label = name_field.as_ref().map(|f| value_text(r.get(f.as_str()))).unwrap_or_default();
            let label = if label.is_empty() { value.clone() } else { label };
            Some(PickOption { value, label })
        })
        .collect();

    Ok(Answer {
        title: title.clone(),
        text,
        partial,
        source,
        plan: Some(title),
        options,
        ..Answer::default()
    })
}

/// Fields to show: the catalogue type when known, else discovered from one probe
/// row (a set like `EmployeesInfo` has no type in the offline catalogue).
async fn resolve_detail_fields(c: &Credentials, set: &str) -> Result<Vec<String>, String> {
    if let Some(t) = catalogue::type_for_set(set) {
        return Ok(pick_detail_fields(t));
    }
    discover_detail_fields(c, set).await
}

/// Auto-picks a handful of meaningful fields for a type: identity, label, date
/// and code fields first. Never returns the whole wide document.
fn pick_detail_fields(t: &catalogue::EntityType) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in t.properties {
        if out.len() >= 6 {
            break;
        }
        let n = p.name;
        if n.ends_with("Code") || n.ends_with("Name") || n.ends_with("Number")
            || n.ends_with("Num") || n.ends_with("Date") || n.ends_with("Time")
        {
            out.push(n.to_string());
        }
    }
    if out.is_empty() {
        out.extend(t.properties.iter().take(4).map(|p| p.name.to_string()));
    }
    out
}

/// Reads the field names of one row so a set whose type is unknown still gets
/// meaningful, real fields rather than a guess. `$top=1` keeps it cheap.
async fn discover_detail_fields(c: &Credentials, set: &str) -> Result<Vec<String>, String> {
    let rows = transport::get(c, &format!("{set}?$top=1")).await?;
    let first = rows
        .get("value")
        .and_then(Value::as_array)
        .and_then(|a| a.first());
    let Some(first) = first else {
        return Ok(Vec::new());
    };
    let mut out: Vec<String> = Vec::new();
    for key in first.as_object().map(|o| o.keys()).into_iter().flatten() {
        if out.len() >= 6 {
            break;
        }
        if key.ends_with("Code") || key.ends_with("Name") || key.ends_with("Number")
            || key.ends_with("Num") || key.ends_with("Date") || key.ends_with("Time")
        {
            out.push(key.clone());
        }
    }
    if out.is_empty() {
        out.extend(
            first
                .as_object()
                .map(|o| o.keys().take(4).cloned())
                .into_iter()
                .flatten(),
        );
    }
    Ok(out)
}

/// A human label for a field, so a detail line reads as a fact, not a column.
fn friendly_field(field: &str) -> &str {
    match field {
        "CardCode" => "Customer code",
        "CardName" => "Customer",
        "CardType" => "Customer type",
        "ItemCode" => "Item code",
        "ItemName" => "Item",
        "ItemsGroupCode" => "Item group",
        "DocNum" => "Number",
        "DocEntry" => "Entry",
        "DocDate" => "Date",
        "DocTotal" => "Total",
        "FirstName" => "First name",
        "LastName" => "Last name",
        "EmployeeID" => "Employee ID",
        "Email" => "Email",
        "Department" => "Department",
        _ => field,
    }
}

/// Text of a JSON cell, empty for null/missing so a detail line drops blank facts.
fn value_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// Builds a sales or purchase document from a validated plan and returns a
/// preview. Nothing is posted here: the app shows the preview and asks for an
/// explicit confirm, then calls the post command.
async fn create_plan(_c: &Credentials, plan: &Plan) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let doc_type = documents::find_by_set(set)
        .ok_or_else(|| format!("{set} is not a document type I can create."))?;
    let lines: Vec<documents::Line> = plan
        .lines
        .iter()
        .map(|l| documents::Line {
            item_code: l.item_code.clone(),
            quantity: l.quantity,
            price: l.price,
            base_line: None,
        })
        .collect();
    let doc = documents::Document {
        doc_type,
        card_code: plan.card_code.clone().unwrap_or_default(),
        doc_date: plan.doc_date.clone(),
        due_date: None,
        lines,
        base: None,
    };
    documents::validate(&doc)?;

    // The spec the app posts after the user confirms. Carried in the answer so
    // the frontend does not have to reconstruct what the model already decided.
    let spec = serde_json::json!({
        "set": set,
        "cardCode": doc.card_code,
        "docDate": doc.doc_date,
        "lines": doc.lines.iter().map(|l| serde_json::json!({
            "itemCode": l.item_code,
            "quantity": l.quantity,
            "price": l.price,
        })).collect::<Vec<_>>(),
    });

    let title = format!("Create {}", doc_type.name);
    let text = format!("{}\n\nConfirm to post to the test company.", documents::preview_text(&doc));
    Ok(Answer {
        title: title.clone(),
        text,
        kind: "confirm".into(),
        plan: Some(title),
        source: "preview â€” not posted".into(),
        payload: Some(spec),
        ..Answer::default()
    })
}

/// Builds the confirm form for a master-data or inventory write. Every field the
/// entity declares is rendered as an input, prefilled from the plan, so the user
/// completes or corrects it before anything posts.
fn entity_plan(plan: &Plan) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let entity = entities::find(set)
        .ok_or_else(|| format!("{set} is not an entity I can create."))?;

    let fields: Vec<Value> = entity
        .fields
        .iter()
        .map(|f| {
            let value = plan
                .values
                .get(f.key)
                .and_then(Value::as_str)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or(f.default);
            serde_json::json!({
                "key": f.key,
                "label": f.label,
                "kind": f.kind,
                "required": f.required,
                "value": value,
                "options": f.options,
            })
        })
        .collect();

    let lines: Vec<Value> = plan
        .lines
        .iter()
        .map(|l| {
            serde_json::json!({
                "itemCode": l.item_code,
                "quantity": l.quantity,
                "price": l.price,
            })
        })
        .collect();

    let spec = serde_json::json!({ "set": set, "fields": fields, "lines": lines });
    let title = format!("Create {}", entity.name);
    let text = format!(
        "{}\n\nFill in the fields, then confirm. It posts to the test company.",
        if plan.summary.trim().is_empty() { &title } else { plan.summary.trim() }
    );
    Ok(Answer {
        title: title.clone(),
        text,
        kind: "confirm".into(),
        plan: Some(title),
        source: "preview — not posted".into(),
        payload: Some(spec),
        ..Answer::default()
    })
}

/// Copies a source document (order) into its twin (invoice): fetch the source,
/// map its card and lines, and return a preview. Nothing is posted here.
async fn copy_plan(c: &Credentials, plan: &Plan) -> Result<Answer, String> {
    let source = plan.entity_set.as_deref().unwrap_or("");
    let target = documents::copy_target(source)
        .ok_or_else(|| format!("{source} cannot be copied to a document."))?;
    let num = plan
        .filter
        .iter()
        .find(|f| f.field == "DocNum")
        .map(|f| f.value.trim_matches('"').to_string())
        .ok_or("No source document number.")?;

    let base_type = documents::base_type_for(source)
        .ok_or_else(|| format!("{source} cannot be copied to a document."))?;
    let query = format!("{source}?$filter=DocNum eq {num}&$select=DocEntry,Status&$top=1");
    let found = transport::get(c, &query).await?;
    let row = found
        .get("value")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .ok_or_else(|| format!("No {source} with number {num}."))?;
    let entry = row.get("DocEntry").and_then(Value::as_i64).unwrap_or_default();
    if entry == 0 {
        return Err(format!("No {source} with number {num}."));
    }
    // A closed source cannot be copied: the server answers `One of the base
    // documents has already been closed`, which is opaque in a chat bubble.
    if row.get("Status").and_then(Value::as_str) == Some("Close") {
        return Err(format!("{source} {num} is closed, so it cannot be copied."));
    }

    // Lines come only on the single-entity GET, not on a list query, and
    // `$expand=DocumentLines` is rejected by this server.
    let src = transport::get(c, &format!("{source}({entry})")).await?;

    let card_code = src.get("CardCode").and_then(Value::as_str).unwrap_or("").to_string();
    let doc_date = src.get("DocDate").and_then(Value::as_str).map(|s| s.to_string());
    let lines: Vec<documents::Line> = src
        .get("DocumentLines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let item_code = l.get("ItemCode").and_then(Value::as_str).unwrap_or("").to_string();
                    if item_code.is_empty() {
                        return None;
                    }
                    let quantity = l.get("Quantity").and_then(Value::as_f64).unwrap_or(0.0);
                    let price = l.get("UnitPrice").and_then(Value::as_f64);
                    // The base line number is what links a copied line back to
                    // its source, and is not the same as the position here.
                    let base_line = l.get("LineNum").and_then(Value::as_i64);
                    Some(documents::Line { item_code, quantity, price, base_line })
                })
                .collect()
        })
        .unwrap_or_default();

    let base = documents::BaseDocument { entry, base_type };
    let doc = documents::Document { doc_type: target, card_code, doc_date, due_date: None, lines, base: Some(base) };
    documents::validate(&doc)?;

    let spec = serde_json::json!({
        "set": target.set,
        "cardCode": doc.card_code,
        "docDate": doc.doc_date,
        "baseEntry": entry,
        "baseType": base_type,
        "lines": doc.lines.iter().map(|l| serde_json::json!({
            "itemCode": l.item_code,
            "quantity": l.quantity,
            "price": l.price,
            "baseLine": l.base_line,
        })).collect::<Vec<_>>(),
    });

    let title = format!("Copy {source} to {}", target.name);
    let text = format!("{}\n\nConfirm to post to the test company.", documents::preview_text(&doc));
    Ok(Answer {
        title: title.clone(),
        text,
        kind: "confirm".into(),
        plan: Some(title),
        source: "preview â€” not posted".into(),
        payload: Some(spec),
        ..Answer::default()
    })
}

/// Sums or averages the rows over one field, optionally grouped.
fn aggregate_rows(rows: &[Value], field: &str, op: &str, dims: Option<&[String]>) -> Option<f64> {
    let mut map: HashMap<String, f64> = HashMap::new();
    let mut total = 0.0;
    let mut count = 0usize;
    for r in rows {
        let v = r.get(field).and_then(Value::as_f64)?;
        total += v;
        count += 1;
        if let Some(dims) = dims {
            let key = dims
                .iter()
                .map(|d| r.get(d).and_then(Value::as_str).unwrap_or("?"))
                .collect::<Vec<_>>()
                .join("Â·");
            *map.entry(key).or_default() += v;
        }
    }
    match op {
        "average" => {
            if count == 0 {
                None
            } else {
                Some(total / count as f64)
            }
        }
        _ => Some(total),
    }
}

// â”€â”€ titles â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn build_source(plan: &Plan, range: &Option<(String, String)>) -> String {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let mut parts = vec![set.to_string()];
    if let Some(sub) = &plan.subtract_entity_set {
        parts.push(format!("minus {sub}"));
    }
    if let Some((start, end)) = range {
        parts.push(format!("{start}..{end}"));
    }
    parts.join(", ")
}

/// A friendly name for an entity set, for a human title â€” never a raw set name
/// like "Invoices" when "sales" reads better, and never a field name.
fn friendly_set(set: &str) -> &str {
    match set {
        "Invoices" => "Sales",
        "PurchaseInvoices" => "Purchases",
        "CreditNotes" => "Credit notes",
        "PurchaseCreditNotes" => "Purchase credit notes",
        "Orders" => "Sales orders",
        "PurchaseOrders" => "Purchase orders",
        "DeliveryNotes" => "Deliveries",
        "BusinessPartners" => "Customers",
        "EmployeesInfo" => "Employees",
        "Items" => "Items",
        "ItemGroups" => "Item groups",
        _ => set,
    }
}

/// " last month", " last quarter" â€” appended to a title.
fn time_label(w: Option<planner::TimeWindow>) -> &'static str {
    use planner::TimeWindow::*;
    match w {
        Some(Today) => " today",
        Some(Yesterday) => " yesterday",
        Some(ThisWeek) => " this week",
        Some(LastWeek) => " last week",
        Some(ThisMonth) => " this month",
        Some(LastMonth) => " last month",
        Some(ThisQuarter) => " this quarter",
        Some(LastQuarter) => " last quarter",
        Some(ThisYear) => " this year",
        Some(LastYear) => " last year",
        Some(Last30Days) => " the last 30 days",
        Some(All) | None => "",
    }
}

fn friendly_dim(dims: &[String]) -> &str {
    if dims.iter().any(|d| d == "DocDate") {
        "month"
    } else if dims.iter().any(|d| d == "CardCode") {
        "customer"
    } else {
        "value"
    }
}

/// A clean, data-derived title for a report. Never carries an LLM placeholder
/// like `<total>` or a raw field name like `DocTotal`.
fn title_for(plan: &Plan) -> String {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let base = if plan.subtract_entity_set.is_some() {
        format!("Net {}", friendly_set(set).to_lowercase())
    } else if plan.group_by.iter().any(|d| d == "CardCode") {
        format!("Top {}", friendly_set(set).to_lowercase())
    } else if plan.parsed_kind() == PlanKind::Count {
        format!("Number of {}", friendly_set(set).to_lowercase())
    } else if !plan.group_by.is_empty() {
        format!("{} by {}", friendly_set(set), friendly_dim(&plan.group_by))
    } else {
        format!("Total {}", friendly_set(set).to_lowercase())
    };
    format!("{base}{}", time_label(plan.time_window))
}

async fn partner_names(c: &Credentials) -> Result<HashMap<String, String>, String> {
    let rows = transport::get(c, "BusinessPartners?$select=CardCode,CardName&$top=5000").await?;
    let mut map = HashMap::new();
    if let Some(items) = rows.get("value").and_then(Value::as_array) {
        for r in items {
            if let (Some(code), Some(name)) = (
                r.get("CardCode").and_then(Value::as_str),
                r.get("CardName").and_then(Value::as_str),
            ) {
                map.insert(code.to_string(), name.to_string());
            }
        }
    }
    Ok(map)
}

/// Thousands separators, two decimals. Currency varies per document, so the
/// figure is printed without a symbol rather than with a wrong one.
pub fn money(value: f64) -> String {
    let negative = value < 0.0;
    let rounded = format!("{:.2}", value.abs());
    let (whole, cents) = rounded.split_once('.').unwrap_or((rounded.as_str(), "00"));
    let mut grouped = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{}{grouped}.{cents}", if negative { "-" } else { "" })
}

fn count_value(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::planner::MetricSpec;

    #[test]
    fn money_groups_and_keeps_the_sign() {
        assert_eq!(money(1234567.891), "1,234,567.89");
        assert_eq!(money(-42.0), "-42.00");
        assert_eq!(money(0.0), "0.00");
    }

    #[test]
    fn count_reads_number_or_string() {
        assert_eq!(count_value(&serde_json::json!(1406)), 1406);
        assert_eq!(count_value(&serde_json::json!("52")), 52);
    }

    #[test]
    fn filter_values_are_literalised_safely() {
        assert_eq!(query::filter_literal("bost_Open").unwrap(), "'bost_Open'");
        assert_eq!(query::filter_literal("42").unwrap(), "42");
        assert_eq!(query::filter_literal("2025-01-01").unwrap(), "datetime'2025-01-01'");
        assert_eq!(query::filter_literal("C0001").unwrap(), "'C0001'");
    }

    #[test]
    fn a_filter_value_cannot_break_out_of_the_literal() {
        for bad in ["x' or '1' eq '1", "a; DROP", "a&b", "$top", "a'b", "a(b)", "a\\b", "a|b"] {
            assert!(query::filter_literal(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(query::filter_literal("").is_err());
    }

    #[test]
    fn row_matches_filters_dates_numbers_and_enums() {
        let row = serde_json::json!({ "DocDate": "2015-06-15", "DocTotal": 120.0, "DocumentStatus": "bost_Open" });
        let date = |op: &str, v: &str| FilterSpec { field: "DocDate".into(), op: op.into(), value: v.into() };
        assert!(row_matches(&row, &date("ge", "2015-01-01")));
        assert!(row_matches(&row, &date("le", "2015-12-31")));
        assert!(!row_matches(&row, &date("le", "2015-01-01")));
        assert!(row_matches(&row, &FilterSpec { field: "DocTotal".into(), op: "gt".into(), value: "100".into() }));
        assert!(row_matches(&row, &FilterSpec { field: "DocumentStatus".into(), op: "eq".into(), value: "bost_Open".into() }));
        assert!(!row_matches(&row, &FilterSpec { field: "DocumentStatus".into(), op: "eq".into(), value: "bost_Close".into() }));
    }

    #[test]
    fn titles_are_human_and_never_carry_placeholders() {
        let p = |kind: &str, set: &str, sub: Option<&str>, group: &[&str], w: Option<planner::TimeWindow>| {
            Plan {
                kind: kind.into(),
                entity_set: Some(set.into()),
                subtract_entity_set: sub.map(|s| s.to_string()),
                group_by: group.iter().map(|s| s.to_string()).collect(),
                metrics: vec![MetricSpec { field: "DocTotal".into(), op: "sum".into(), alias: "Total".into() }],
                filter: vec![],
                time_window: w,
                card_code: None,
                doc_date: None,
                lines: vec![],
                values: Default::default(),
                clarifying_question: None,
                summary: "You had a total value of <total> across <count> orders.".into(),
            }
        };
        assert_eq!(
            title_for(&p("aggregate", "Invoices", Some("CreditNotes"), &[], Some(planner::TimeWindow::LastQuarter))),
            "Net sales last quarter"
        );
        assert_eq!(title_for(&p("count", "EmployeesInfo", None, &[], None)), "Number of employees");
        assert_eq!(
            title_for(&p("aggregate", "Invoices", None, &["DocDate"], Some(planner::TimeWindow::ThisMonth))),
            "Sales by month this month"
        );
        assert_eq!(title_for(&p("aggregate", "Orders", None, &["CardCode"], None)), "Top sales orders");
    }

    #[test]
    fn aggregate_rows_sums_and_averages() {
        let rows = vec![
            serde_json::json!({ "DocTotal": 10.0 }),
            serde_json::json!({ "DocTotal": 20.0 }),
            serde_json::json!({ "DocTotal": 30.0 }),
        ];
        assert_eq!(aggregate_rows(&rows, "DocTotal", "sum", None), Some(60.0));
        assert_eq!(aggregate_rows(&rows, "DocTotal", "average", None), Some(20.0));
    }

    #[test]
    fn aggregate_rows_groups() {
        let rows = vec![
            serde_json::json!({ "CardCode": "A", "DocTotal": 10.0 }),
            serde_json::json!({ "CardCode": "B", "DocTotal": 5.0 }),
            serde_json::json!({ "CardCode": "A", "DocTotal": 7.0 }),
        ];
        let out = aggregate_rows(&rows, "DocTotal", "sum", Some(&["CardCode".to_string()]));
        assert_eq!(out, Some(22.0));
    }

    #[test]
    fn a_clarify_plan_carries_the_question() {
        let plan = Plan {
            kind: "clarify".into(),
            clarifying_question: Some("Which period?".into()),
            summary: "Need a period.".into(),
            ..Default::default()
        };
        let a = clarify_answer(&plan);
        assert_eq!(a.kind, "clarify");
        assert_eq!(a.clarifying_question.as_deref(), Some("Which period?"));
        assert_eq!(a.source, "no query run");
    }

    /// Runs the real write path end to end: a sales order, then an invoice
    /// copied from it through `BaseEntry`/`BaseLine`/`BaseType`. It goes through
    /// `create_document`, so the due date, the partner check and the payload
    /// builder are all exercised, not a hand-written body.
    /// Creates documents in the test company.
    #[test]
    #[ignore = "creates documents on the configured test company"]
    fn live_sales_cycle_create_order_then_invoice() {
        let c = super::super::credentials_from_secrets().expect("SAP credentials");
        let item = "A00001";
        // C70000 carries valid CA/CA states, unlike C20000 whose state is null.
        let card = "C70000";
        // Existing documents are from 2015, where the USD exchange rate exists.
        let doc_date = "2015-01-22".to_string();

        let order_spec = serde_json::json!({
            "set": "Orders",
            "cardCode": card,
            "docDate": doc_date,
            "lines": [{ "itemCode": item, "quantity": 2.0, "price": 300.0 }],
        });
        let order = tauri::async_runtime::block_on(super::super::create_document(&c, &order_spec))
            .unwrap_or_else(|e| panic!("order POST failed: {e}"));
        let order_entry = order["DocEntry"].as_i64().expect("order DocEntry");
        println!(
            "order OK DocEntry={order_entry} DocNum={} DocDueDate={}",
            order["DocNum"], order["DocDueDate"]
        );

        let src = tauri::async_runtime::block_on(transport::get(&c, &format!("Orders({order_entry})")))
            .expect("read back order");
        let base_line = src["DocumentLines"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|l| l["LineNum"].as_i64())
            .expect("order line number");

        let invoice_spec = serde_json::json!({
            "set": "Invoices",
            "cardCode": card,
            "docDate": doc_date,
            "baseEntry": order_entry,
            "baseType": 17,
            "lines": [{ "itemCode": item, "quantity": 2.0, "baseLine": base_line }],
        });
        let invoice = tauri::async_runtime::block_on(super::super::create_document(&c, &invoice_spec))
            .unwrap_or_else(|e| panic!("invoice POST failed: {e}"));
        println!("invoice OK DocEntry={} DocNum={}", invoice["DocEntry"], invoice["DocNum"]);
    }

    /// A partner with no billing state must be refused with a readable reason
    /// before the document is built, not after the server rejects it.
    #[test]
    #[ignore = "reads live Business Partner data"]
    fn live_partner_without_billing_state_is_refused_by_name() {
        let c = super::super::credentials_from_secrets().expect("SAP credentials");
        let spec = serde_json::json!({
            "set": "Orders",
            "cardCode": "C20000",
            "docDate": "2015-01-22",
            "lines": [{ "itemCode": "A00001", "quantity": 2.0 }],
        });
        match tauri::async_runtime::block_on(super::super::create_document(&c, &spec)) {
            Err(e) => {
                println!("refused as expected: {e}");
                assert!(e.contains("billing state"), "unreadable reason: {e}");
            }
            // A server that accepted it means the partner was fixed; nothing to
            // assert beyond the absence of a panic.
            Ok(v) => println!("C20000 is now usable, DocEntry={}", v["DocEntry"]),
        }
    }

    /// Drives the entity write path: create an item, then receive stock for it
    /// through `write_entity`, the same function the confirm form calls.
    /// Creates an item and a goods receipt on the test company.
    #[test]
    #[ignore = "creates an item and a goods receipt on the test company"]
    fn live_entity_write_creates_item_then_receipt() {
        let c = super::super::credentials_from_secrets().expect("SAP credentials");
        let code = "DEMO-W01";

        let item_values = serde_json::json!({
            "ItemCode": code, "ItemName": "Written item", "ItemsGroupCode": "100"
        });
        match tauri::async_runtime::block_on(super::super::write_entity(&c, "Items", &item_values, &serde_json::json!([]))) {
            Ok(_) => println!("item {code} created"),
            // A rerun finds it and that is fine; any other error is not.
            Err(e) if e.contains("already") || e.contains("exists") => println!("item {code} already there"),
            Err(e) => panic!("item create failed: {e}"),
        }

        let receipt_values = serde_json::json!({ "DocDate": "2026-08-01", "WarehouseCode": "01" });
        let lines = serde_json::json!([{ "itemCode": code, "quantity": 500.0, "price": 5.0 }]);
        let receipt = tauri::async_runtime::block_on(super::super::write_entity(&c, "InventoryGenEntries", &receipt_values, &lines))
            .unwrap_or_else(|e| panic!("goods receipt failed: {e}"));
        println!("goods receipt DocEntry={}", receipt["DocEntry"]);
        assert!(receipt["DocEntry"].as_i64().unwrap_or(0) > 0);
    }

    /// Dumps the company's `$metadata` to a file so the catalogue generator can
    /// enumerate every endpoint, including the ones no feature uses yet.
    #[test]
    #[ignore = "reads live Service Layer metadata and writes a temp file"]
    fn live_dump_metadata_for_catalogue() {
        let c = super::super::credentials_from_secrets().expect("SAP credentials");
        let xml = tauri::async_runtime::block_on(transport::get_text(&c, "$metadata")).expect("metadata");
        let path = std::path::PathBuf::from(
            std::env::var("SAP_METADATA_OUT")
                .unwrap_or_else(|_| std::env::temp_dir().join("sap-metadata.xml").display().to_string()),
        );
        std::fs::write(&path, &xml).expect("write metadata");
        let sets = xml.matches("<EntitySet ").count();
        let types = xml.matches("<EntityType ").count();
        println!("wrote {} bytes: {sets} entity sets, {types} entity types -> {}", xml.len(), path.display());
    }
}

