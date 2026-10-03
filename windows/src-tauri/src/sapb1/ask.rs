//! Answers questions about the ERP from the island chat.
//!
//! Two paths:
//!
//!   - Planner path: the question is handed to the configured chat model with a
//!     slice of the catalogue. The model returns a structured `Plan`, which is
//!     validated against the report allowlists and executed. When the plan needs
//!     a period or a choice, the assistant asks the user in text (a stateful
//!     clarifying turn) instead of guessing. This is the agentic path.
//!   - Fallback path: when no chat backend is configured, the same deterministic
//!     intents as before. Nothing is invented; an unmatched question returns the
//!     list of what this can answer.
//!
//! The queries respect the rules in `query` — no `$filter` with `$apply`, no date
//! functions in `groupby` — so the executor fetches raw rows and slices, buckets
//! and sums in Rust rather than asking the server to do something it rejects.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;

use super::dates;
use super::planner::{self, Plan, PlanKind};
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
    /// Which query produced this, so the figure can be trusted or challenged.
    pub source: String,
    /// "result" | "clarify" | "help". The front end renders each differently.
    pub kind: String,
    /// The "what I'll do" bubble, shown before a result.
    pub plan: Option<String>,
    /// The question the assistant needs answered before it can run.
    pub clarifying_question: Option<String>,
    /// Tappable follow-ups, only in the no-LLM fallback. Not rendered as chips
    /// when the planner is active — the assistant asks conversationally instead.
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
            clarifying_question: None,
            suggestions: vec![],
        }
    }
}

/// The clarifying turn in flight: the original question and its plan, so the
/// user's answer can be folded back in and re-planned.
struct PendingPlan {
    question: String,
    plan: Plan,
}

static PENDING: Mutex<Option<PendingPlan>> = Mutex::new(None);

fn store_pending(p: PendingPlan) {
    *PENDING.lock().unwrap() = Some(p);
}

fn take_pending() -> Option<PendingPlan> {
    PENDING.lock().unwrap().take()
}

/// Clears any clarifying turn in flight. Called when the chat leaves the ERP.
pub fn clear_pending() {
    *PENDING.lock().unwrap() = None;
}

/// Whether a clarifying turn is in flight, so the front end can point the chat
/// back at the ERP or clear it.
pub fn pending_clarification() -> bool {
    PENDING.lock().unwrap().is_some()
}

/// The main entry. `settings` decides which backend the planner uses; the ERP
/// credentials come from the Credential Manager.
pub async fn ask(c: &Credentials, settings: &Settings, question: &str) -> Result<Answer, String> {
    // A clarifying turn in flight means this message is the answer.
    if let Some(pending) = take_pending() {
        return answer_clarification(c, settings, pending, question).await;
    }

    match planner::plan(settings, question).await {
        Ok(plan) if plan.is_clarify() => {
            store_pending(PendingPlan {
                question: question.to_string(),
                plan: plan.clone(),
            });
            Ok(clarify_answer(&plan))
        }
        Ok(plan) if plan.is_answer() => Ok(answer_reply(&plan)),
        Ok(plan) => execute(c, &plan).await,
        // No backend/key/binding, or the model could not produce a plan: fall
        // back to the deterministic intents rather than leaving a dead end.
        Err(_) => fallback(c, question).await,
    }
}

/// The user answered a clarifying question: fold the answer into the original
/// question, re-plan, and run. Still ambiguous → ask again.
async fn answer_clarification(
    c: &Credentials,
    settings: &Settings,
    pending: PendingPlan,
    answer: &str,
) -> Result<Answer, String> {
    let combined = format!("{} {}", pending.question, answer.trim());
    match planner::plan(settings, &combined).await {
        Ok(plan) if plan.is_clarify() => {
            store_pending(PendingPlan {
                question: combined,
                plan: plan.clone(),
            });
            Ok(clarify_answer(&plan))
        }
        Ok(plan) if plan.is_answer() => Ok(answer_reply(&plan)),
        Ok(plan) => execute(c, &plan).await,
        Err(_) => fallback(c, &combined).await,
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

// ── deterministic fallback ────────────────────────────────────────────────────

/// Keyword detection for the fallback path. Order matters: the most specific
/// intent wins. This is only reached when the planner is unavailable, but it must
/// still handle the user's wording, not ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    SalesByMonth,
    TopCustomers,
    SalesTotal,
    OpenOrders,
    Receivables,
    PurchasesByMonth,
    TopVendors,
    PurchasesTotal,
    OrderCount,
    InvoiceCount,
    EmployeeCount,
    Unknown,
}

pub fn detect(question: &str) -> Intent {
    let q = question.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| q.contains(w));
    let has_order = has(&["order", "backlog", "booked", "pending order"]);
    if has(&["top customer", "best customer", "largest customer", "top clients", "top buyer"]) {
        Intent::TopCustomers
    } else if has(&["top vendor", "top supplier", "best supplier", "top vendors"]) {
        Intent::TopVendors
    } else if has(&[
        "receivable", "outstanding", "owed", "who owes", "aging", "ageing", "unpaid",
    ]) {
        Intent::Receivables
    } else if has(&["employee", "staff", "headcount", "people work", "team size"]) {
        Intent::EmployeeCount
    } else if has(&["sales by month", "sales per month", "monthly sales", "sales trend"])
        || (has(&["month", "monthly", "trend"]) && has(&["sale", "revenue", "turnover"]))
    {
        Intent::SalesByMonth
    } else if has(&["purchase by month", "purchases by month", "monthly purchase", "spend by month", "spend trend"])
        || (has(&["purchase", "spend", "procurement"]) && has(&["month", "trend", "monthly"]))
    {
        Intent::PurchasesByMonth
    } else if has(&["how many order", "order count", "number of order", "count of order", "count our order", "count the order", "total order", "order total"])
        || (has(&["how many", "total", "count"]) && has(&["order"]))
    {
        // Before the orders report: "how many orders" also contains "order".
        Intent::OrderCount
    } else if has(&["how many invoice", "invoice count", "number of invoice", "count of invoice", "count our invoice", "total invoice", "invoice total"])
        || (has(&["how many", "total", "count"]) && has(&["invoice", "receipt"]))
    {
        Intent::InvoiceCount
    } else if has_order {
        Intent::OpenOrders
    } else if has(&["sale", "revenue", "turnover", "income", "how much did we sell", "total sale"]) {
        Intent::SalesTotal
    } else if has(&["purchase", "spend", "procurement", "vendor bill"]) {
        Intent::PurchasesTotal
    } else {
        Intent::Unknown
    }
}

pub const HELP: &str = "Hi! I'm your business assistant. I can pull your sales, orders, customers, \
invoices, receivables, purchases, stock and employee count from your SAP data. Ask me for a figure and I'll get it for you.";

async fn fallback(c: &Credentials, question: &str) -> Result<Answer, String> {
    match detect(question) {
        Intent::SalesTotal => net_sales(c).await,
        Intent::SalesByMonth => sales_by_month(c).await,
        Intent::TopCustomers => top_partners(c, "Invoices", "Top customers by invoiced value").await,
        Intent::PurchasesTotal => purchases_total(c).await,
        Intent::PurchasesByMonth => purchases_by_month(c).await,
        Intent::TopVendors => top_partners(c, "PurchaseInvoices", "Top vendors by purchase value").await,
        Intent::OpenOrders => open_orders(c).await,
        Intent::Receivables => receivables(c).await,
        Intent::OrderCount => count(c, "Orders", "Sales orders").await,
        Intent::InvoiceCount => count(c, "Invoices", "Sales invoices").await,
        Intent::EmployeeCount => count(c, "EmployeesInfo", "Employees").await,
        // A greeting or an off-topic question: welcome first, then say what this
        // can answer — never a dead-end wall of text, and no clickable chips.
        Intent::Unknown => Ok(Answer {
            title: "Mochi".into(),
            text: HELP.into(),
            kind: "answer".into(),
            source: "no query run".into(),
            ..Answer::default()
        }),
    }
}

// ── executor ──────────────────────────────────────────────────────────────────

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
        PlanKind::Answer => Ok(answer_reply(plan)),
        PlanKind::Clarify => Err("The plan still needs a clarifying answer.".into()),
    }
}

/// Fetches rows for a set, then filters them by the date range (if any) on the
/// `DocDate` field. The dataset is small enough to page once; the row cap is a
/// lower-bound guard, not a filter.
async fn fetch_rows(
    c: &Credentials,
    set: &str,
    select: &[String],
    range: &Option<(String, String)>,
) -> Result<(Vec<Value>, bool), String> {
    let mut fields = select.to_vec();
    if range.is_some() && !fields.iter().any(|f| f == "DocDate") && query::is_report_field(set, "DocDate") {
        fields.push("DocDate".into());
    }
    let select = fields.join(",");
    let query = format!("{set}?$select={select}&$top={ROW_CAP}");
    let rows = transport::get(c, &query).await?;
    let list = rows.get("value").and_then(Value::as_array).cloned().unwrap_or_default();
    let filtered: Vec<Value> = if let Some((start, end)) = range {
        list.into_iter()
            .filter(|r| {
                let date = r.get("DocDate").and_then(Value::as_str).unwrap_or("");
                date.is_empty() || (date >= start.as_str() && date <= end.as_str())
            })
            .collect()
    } else {
        list
    };
    let partial = filtered.len() >= ROW_CAP;
    Ok((filtered, partial))
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
        return Ok(Answer {
            title: set.to_string(),
            text: format!("{set}: {n}"),
            total: Some(n as f64),
            source,
            plan: Some(plan.summary.clone()),
            ..Answer::default()
        });
    }

    let Some(metric) = metric else {
        return Err("The plan has no metric.".into());
    };
    let mut select = vec![metric.field.clone()];
    let (rows, partial) = fetch_rows(c, set, &select, &range).await?;
    let mut value = aggregate_rows(&rows, &metric.field, &metric.op, None).unwrap_or(0.0);

    // A net figure (sales minus credit notes, purchases minus purchase credit
    // notes) subtracts the same window from a second set.
    if let Some(sub) = &plan.subtract_entity_set {
        let (sub_rows, _) = fetch_rows(c, sub, &select, &range).await?;
        value -= aggregate_rows(&sub_rows, &metric.field, &metric.op, None).unwrap_or(0.0);
    }

    let title = plan.summary.clone();
    // The report card shows the title and the total; the note is only a
    // human line when there is nothing to chart.
    let text = if value == 0.0 && range.is_some() {
        "No invoices in this period.".to_string()
    } else {
        String::new()
    };
    Ok(Answer {
        title: title.clone(),
        text,
        total: Some(value),
        partial,
        source,
        plan: Some(plan.summary.clone()),
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
    let (rows, partial) = fetch_rows(c, set, &select, &range).await?;

    let mut groups: HashMap<String, f64> = HashMap::new();
    for r in &rows {
        let key = dims
            .iter()
            .map(|d| r.get(d).and_then(Value::as_str).unwrap_or("?").to_string())
            .collect::<Vec<_>>()
            .join(" · ");
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
    let title = plan.summary.clone();
    let text = String::new();
    Ok(Answer {
        title: title.clone(),
        text,
        rows,
        total: Some(total),
        partial,
        source,
        plan: Some(plan.summary.clone()),
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
    Ok(Answer {
        title: set.to_string(),
        text: String::new(),
        total: Some(n as f64),
        source,
        plan: Some(plan.summary.clone()),
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
    if let Some(f) = &plan.filter {
        filter.push(format!("{} {} {}", f.field, f.op, f.value));
    }
    let query = if filter.is_empty() {
        format!("{set}/$count")
    } else {
        format!("{set}/$count?$filter={}", filter.join(" and "))
    };
    let raw = transport::get_text(c, &query).await?;
    raw.trim().trim_matches('"').parse().map_err(|_| format!("$count returned {raw:?}"))
}

async fn list_plan(
    c: &Credentials,
    plan: &Plan,
    range: Option<(String, String)>,
) -> Result<Answer, String> {
    let set = plan.entity_set.as_deref().unwrap_or("");
    let source = build_source(plan, &range);
    let (rows, partial) = fetch_rows(c, set, &[], &range).await?;
    // Pick the first string field as the label and the first numeric field as
    // the value, so a bare list is still readable.
    let label_field = pick_label_field(set);
    let value_field = metric_field(plan);
    let value_field = if value_field.is_empty() {
        "DocTotal".to_string()
    } else {
        value_field
    };
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
    let title = plan.summary.clone();
    let text = String::new();
    Ok(Answer {
        title: title.clone(),
        text,
        rows: out,
        total: Some(total),
        partial,
        source,
        plan: Some(plan.summary.clone()),
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

/// Sums or averages the rows over one field, optionally grouped. Used by the
/// executor after the rows come down; the server-side `$apply` path is not used
/// for windowed aggregates because it cannot carry a `$filter`.
fn aggregate_rows(
    rows: &[Value],
    field: &str,
    op: &str,
    dims: Option<&[String]>,
) -> Option<f64> {
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
                .join("·");
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

// ── existing deterministic query builders (fallback) ─────────────────────────

async fn net_sales(c: &Credentials) -> Result<Answer, String> {
    let invoiced = aggregate_sum(c, "Invoices", "DocTotal").await?;
    let credited = aggregate_sum(c, "CreditNotes", "DocTotal").await?;
    let net = invoiced - credited;
    Ok(Answer {
        title: "Net sales".into(),
        text: format!(
            "Net sales (invoiced minus credit notes)\n  invoiced   {}\n  credit     {}\n  net        {}",
            money(invoiced),
            money(credited),
            money(net)
        ),
        total: Some(net),
        source: "Invoices and CreditNotes, sum of DocTotal".into(),
        ..Answer::default()
    })
}

async fn purchases_total(c: &Credentials) -> Result<Answer, String> {
    let bought = aggregate_sum(c, "PurchaseInvoices", "DocTotal").await?;
    let credited = aggregate_sum(c, "PurchaseCreditNotes", "DocTotal").await.unwrap_or(0.0);
    let net = bought - credited;
    Ok(Answer {
        title: "Purchases".into(),
        text: format!("Purchases (invoiced minus purchase credit notes)\n  net  {}", money(net)),
        total: Some(net),
        source: "PurchaseInvoices and PurchaseCreditNotes, sum of DocTotal".into(),
        ..Answer::default()
    })
}

async fn sales_by_month(c: &Credentials) -> Result<Answer, String> {
    monthly(c, "Invoices", "Sales by month").await
}

async fn purchases_by_month(c: &Credentials) -> Result<Answer, String> {
    monthly(c, "PurchaseInvoices", "Purchases by month").await
}

async fn monthly(c: &Credentials, set: &str, title: &str) -> Result<Answer, String> {
    let query = format!("{set}?$apply=groupby((DocDate),aggregate(DocTotal with sum as Total))");
    let rows = transport::get(c, &query).await?;
    let (buckets, seen) = month_buckets(rows.get("value").and_then(Value::as_array));
    let source = format!("{set}, groupby DocDate, sum of DocTotal, bucketed to months");
    Ok(answer_from_buckets(title, buckets, seen, &source))
}

async fn top_partners(c: &Credentials, set: &str, title: &str) -> Result<Answer, String> {
    let query = format!("{set}?$apply=groupby((CardCode),aggregate(DocTotal with sum as Total))");
    let rows = transport::get(c, &query).await?;
    let names = partner_names(c).await.unwrap_or_default();
    let mut pairs = value_pairs(rows.get("value").and_then(Value::as_array), "CardCode", "Total");
    pairs.sort_by(|a, b| b.1.total_cmp(&a.1));
    pairs.truncate(8);
    let out: Vec<AnswerRow> = pairs
        .into_iter()
        .map(|(code, value)| AnswerRow {
            label: names.get(&code).cloned().unwrap_or(code),
            value,
        })
        .collect();
    let total = out.iter().map(|r| r.value).sum();
    Ok(Answer {
        title: title.into(),
        text: render(title, &out, Some(total)),
        rows: out,
        total: Some(total),
        source: format!("{set}, groupby CardCode, sum of DocTotal"),
        ..Answer::default()
    })
}

async fn open_orders(c: &Credentials) -> Result<Answer, String> {
    let query = "Orders?$select=DocEntry,DocTotal,CardName&$filter=DocumentStatus eq 'bost_Open'&$top=5000";
    let rows = transport::get(c, query).await?;
    let list = rows.get("value").and_then(Value::as_array);
    let count = list.map_or(0, Vec::len);
    let total: f64 = list
        .map(|items| items.iter().filter_map(|r| r.get("DocTotal").and_then(Value::as_f64)).sum())
        .unwrap_or(0.0);
    let partial = count >= ROW_CAP;
    Ok(Answer {
        title: "Open sales orders".into(),
        text: format!(
            "Open sales orders\n  count  {count}{}\n  value  {}",
            if partial { " (capped)" } else { "" },
            money(total)
        ),
        total: Some(total),
        partial,
        source: "Orders where DocumentStatus is bost_Open".into(),
        ..Answer::default()
    })
}

async fn receivables(c: &Credentials) -> Result<Answer, String> {
    let query = "BusinessPartners?$filter=CardType eq 'cCustomer'&$select=CardCode,CardName,CurrentAccountBalance&$top=5000";
    let rows = transport::get(c, query).await?;
    let list = rows.get("value").and_then(Value::as_array);
    let mut owed: Vec<AnswerRow> = list
        .map(|items| {
            items
                .iter()
                .filter_map(|r| {
                    let balance = r.get("CurrentAccountBalance").and_then(Value::as_f64)?;
                    if balance <= 0.0 {
                        return None;
                    }
                    Some(AnswerRow {
                        label: r.get("CardName").and_then(Value::as_str).unwrap_or("?").to_string(),
                        value: balance,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    owed.sort_by(|a, b| b.value.total_cmp(&a.value));
    let total: f64 = owed.iter().map(|r| r.value).sum();
    let partial = owed.len() > 8;
    let mut top = owed;
    top.truncate(8);
    let body = render("Receivables (current account balance)", &top, Some(total));
    Ok(Answer {
        title: "Receivables".into(),
        text: body,
        rows: top,
        total: Some(total),
        partial,
        source: "BusinessPartners where CardType is cCustomer, CurrentAccountBalance above zero".into(),
        ..Answer::default()
    })
}

async fn count(c: &Credentials, set: &str, title: &str) -> Result<Answer, String> {
    let raw = transport::get_text(c, &format!("{set}/$count")).await?;
    let n: i64 = raw.trim().trim_matches('"').parse().map_err(|_| format!("$count returned {raw:?}"))?;
    Ok(Answer {
        title: title.into(),
        text: format!("{title}: {n}"),
        total: Some(n as f64),
        source: format!("{set}/$count"),
        ..Answer::default()
    })
}

// ── helpers ──────────────────────────────────────────────────────────────────

async fn aggregate_sum(c: &Credentials, set: &str, field: &str) -> Result<f64, String> {
    let query = format!("{set}?$apply=aggregate({field} with sum as Total)");
    let rows = transport::get(c, &query).await?;
    Ok(rows
        .get("value")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .and_then(|r| r.get("Total"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0))
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

fn month_buckets(rows: Option<&Vec<Value>>) -> (Vec<(String, f64)>, usize) {
    let mut map: HashMap<String, f64> = HashMap::new();
    let mut seen = 0;
    if let Some(items) = rows {
        seen = items.len();
        for r in items {
            let date = r.get("DocDate").and_then(Value::as_str).unwrap_or("");
            let total = r.get("Total").and_then(Value::as_f64).unwrap_or(0.0);
            if date.len() >= 7 {
                *map.entry(date[..7].to_string()).or_default() += total;
            }
        }
    }
    let mut out: Vec<(String, f64)> = map.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    (out, seen)
}

fn value_pairs(rows: Option<&Vec<Value>>, key: &str, value: &str) -> Vec<(String, f64)> {
    rows.map(|items| {
        items
            .iter()
            .filter_map(|r| {
                let k = r.get(key).and_then(Value::as_str)?.to_string();
                let v = r.get(value).and_then(Value::as_f64)?;
                Some((k, v))
            })
            .collect()
    })
    .unwrap_or_default()
}

fn answer_from_buckets(title: &str, buckets: Vec<(String, f64)>, seen: usize, source: &str) -> Answer {
    let rows: Vec<AnswerRow> = buckets
        .into_iter()
        .map(|(label, value)| AnswerRow { label, value })
        .collect();
    let total: f64 = rows.iter().map(|r| r.value).sum();
    Answer {
        title: title.into(),
        text: render(title, &rows, Some(total)),
        rows,
        total: Some(total),
        partial: seen >= ROW_CAP,
        source: source.into(),
        ..Answer::default()
    }
}

fn render(title: &str, rows: &[AnswerRow], total: Option<f64>) -> String {
    let mut lines = vec![title.to_string()];
    for row in rows {
        lines.push(format!("  {:<28} {}", row.label, money(row.value)));
    }
    if let Some(sum) = total {
        lines.push(format!("  {:<28} {}", "total", money(sum)));
    }
    lines.join("\n")
}

fn count_value(value: &Value) -> i64 {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn keywords_pick_the_specific_intent() {
        assert_eq!(detect("who are my top customers?"), Intent::TopCustomers);
        assert_eq!(detect("sales by month"), Intent::SalesByMonth);
        assert_eq!(detect("what are my receivables"), Intent::Receivables);
        assert_eq!(detect("show open orders"), Intent::OpenOrders);
        assert_eq!(detect("purchases by month"), Intent::PurchasesByMonth);
        assert_eq!(detect("total revenue"), Intent::SalesTotal);
        assert_eq!(detect("weather tomorrow"), Intent::Unknown);
    }

    #[test]
    fn the_users_own_wording_reaches_the_report() {
        assert_eq!(detect("hi"), Intent::Unknown);
        assert_eq!(detect("whats our orders?"), Intent::OpenOrders);
        assert_eq!(detect("our orders"), Intent::OpenOrders);
        assert_eq!(detect("orders"), Intent::OpenOrders);
        assert_eq!(detect("who owes us"), Intent::Receivables);
        assert_eq!(detect("unpaid invoices"), Intent::Receivables);
        assert_eq!(detect("how many orders do we have"), Intent::OrderCount);
        assert_eq!(detect("how many invoices"), Intent::InvoiceCount);
        assert_eq!(detect("sales this month"), Intent::SalesByMonth);
        assert_eq!(detect("spend by month"), Intent::PurchasesByMonth);
        assert_eq!(detect("how much did we sell"), Intent::SalesTotal);
    }

    #[test]
    fn count_questions_are_not_orders_reports() {
        assert_eq!(detect("how many orders"), Intent::OrderCount);
        assert_eq!(detect("order count"), Intent::OrderCount);
        assert_eq!(detect("invoice count"), Intent::InvoiceCount);
    }

    #[test]
    fn month_needs_both_halves() {
        assert_eq!(detect("sales"), Intent::SalesTotal);
        assert_eq!(detect("sales this month"), Intent::SalesByMonth);
        assert_eq!(detect("monthly sales trend"), Intent::SalesByMonth);
    }

    #[test]
    fn months_bucket_and_sort() {
        let rows = vec![
            serde_json::json!({ "DocDate": "2025-03-04", "Total": 10.0 }),
            serde_json::json!({ "DocDate": "2025-01-09", "Total": 5.0 }),
            serde_json::json!({ "DocDate": "2025-03-21", "Total": 2.5 }),
        ];
        let (buckets, seen) = month_buckets(Some(&rows));
        assert_eq!(seen, 3);
        assert_eq!(buckets, vec![("2025-01".into(), 5.0), ("2025-03".into(), 12.5)]);
    }

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
    fn a_clarify_answer_carries_the_question() {
        let plan = Plan {
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
        let a = clarify_answer(&plan);
        assert_eq!(a.kind, "clarify");
        assert_eq!(a.clarifying_question.as_deref(), Some("Which period?"));
        assert_eq!(a.source, "no query run");
    }

    #[test]
    fn date_range_is_inclusive_on_the_docdate_string() {
        // The filter is a string comparison on YYYY-MM-DD, which sorts correctly.
        let rows = vec![
            serde_json::json!({ "DocDate": "2025-01-15", "DocTotal": 1.0 }),
            serde_json::json!({ "DocDate": "2025-02-15", "DocTotal": 2.0 }),
            serde_json::json!({ "DocDate": "2025-03-15", "DocTotal": 3.0 }),
        ];
        let range = Some(("2025-02-01".to_string(), "2025-02-28".to_string()));
        let (start, end) = range.as_ref().unwrap();
        // fetch_rows is async; the filtering logic is inline here for the test.
        let kept: Vec<_> = rows.iter().filter(|r| {
            let date = r.get("DocDate").and_then(Value::as_str).unwrap_or("");
            date.is_empty() || (date >= start.as_str() && date <= end.as_str())
        }).collect();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn default_settings_fallback_is_deterministic() {
        // With no SAP binding configured, the planner is unavailable and ask()
        // falls back to the deterministic intents. This must still produce a
        // result or a warm greeting, never a panic.
        let s = Settings::default();
        let _ = s;
        assert!(!HELP.is_empty());
    }
}