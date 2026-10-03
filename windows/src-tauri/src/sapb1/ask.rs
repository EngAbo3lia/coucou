//! Answers questions about the ERP from the island chat.
//!
//! The model is the brain. A question plus the conversation so far goes to the
//! configured chat backend, which returns a structured `Plan`. The plan is
//! validated against the report allowlists and executed. Nothing here does
//! keyword routing: the model decides what to ask, what to run, and how to
//! answer. The only hard guard is the validator — a plan can never read a field
//! outside the allowlist, and filter values are sanitised before they reach the
//! query string.
//!
//! The queries respect the rules in `query` — no `$filter` with `$apply`, no date
//! functions in `groupby` — so the executor fetches raw rows and slices, buckets
//! and sums in Rust rather than asking the server to do something it rejects.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::dates;
use super::planner::{self, ChatTurn, Plan, PlanKind};
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
    /// Which data produced this, so the figure can be trusted or challenged.
    pub source: String,
    /// "result" | "clarify" | "answer".
    pub kind: String,
    /// The "what I'll do" bubble, shown before a result.
    pub plan: Option<String>,
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
            "No AI backend is set up for the SAP Harness. Configure one in Settings → Agents, then ask again.".to_string()
        } else if e.contains("Bad plan") || e.contains("no JSON") {
            "I couldn't work out how to answer that one. Try asking it a different way.".to_string()
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

/// A filter value as a safe OData literal. The field and the operator are
/// validated by `Plan::validated()`; the value is checked here so a value can
/// never break out of the literal or append a clause.
fn filter_literal(value: &str) -> Result<String, String> {
    let v = value.trim();
    if v.is_empty() {
        return Err("Empty filter value.".into());
    }
    // Anything that could close the literal or start a new clause is refused.
    if v.chars()
        .any(|c| matches!(c, '\'' | '"' | ';' | '&' | '%' | '$' | '(' | ')' | '\\' | '|'))
    {
        return Err(format!("Unsafe filter value {v:?}."));
    }
    if is_iso_date(v) {
        // `datetime'...'`, the form verified against the live server.
        return Ok(format!("datetime'{v}'"));
    }
    if v.parse::<f64>().is_ok() {
        return Ok(v.to_string());
    }
    // A plain string literal (an enum like bost_Open, a status word, a code).
    if v.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ' | '/' | ':' | '@'))
    {
        Ok(format!("'{v}'"))
    } else {
        Err(format!("Unsafe filter value {v:?}."))
    }
}

fn is_iso_date(v: &str) -> bool {
    v.len() == 10
        && v.as_bytes()[4] == b'-'
        && v.as_bytes()[7] == b'-'
        && v[..4].bytes().all(|b| b.is_ascii_digit())
        && v[5..7].bytes().all(|b| b.is_ascii_digit())
        && v[8..].bytes().all(|b| b.is_ascii_digit())
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
    let query = if select.is_empty() {
        format!("{set}?$top={ROW_CAP}")
    } else {
        format!("{set}?$select={select}&$top={ROW_CAP}")
    };
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
    let (rows, partial) = fetch_rows(c, set, &select, &range).await?;
    let mut value = aggregate_rows(&rows, &metric.field, &metric.op, None).unwrap_or(0.0);

    // A net figure (sales minus credit notes, purchases minus purchase credit
    // notes) subtracts the same window from a second set.
    if let Some(sub) = &plan.subtract_entity_set {
        let (sub_rows, _) = fetch_rows(c, sub, &select, &range).await?;
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
    if let Some(f) = &plan.filter {
        // The field and op were validated; the value is sanitised here.
        filter.push(format!("{} {} {}", f.field, f.op, filter_literal(&f.value)?));
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
    let label_field = pick_label_field(set);
    let value_field = metric_field(plan);
    let value_field = if value_field.is_empty() { "DocTotal".to_string() } else { value_field };
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

// ── titles ────────────────────────────────────────────────────────────────────

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

/// A friendly name for an entity set, for a human title — never a raw set name
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

/// " last month", " last quarter" — appended to a title.
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
        assert_eq!(filter_literal("bost_Open").unwrap(), "'bost_Open'");
        assert_eq!(filter_literal("42").unwrap(), "42");
        assert_eq!(filter_literal("2025-01-01").unwrap(), "datetime'2025-01-01'");
        assert_eq!(filter_literal("C0001").unwrap(), "'C0001'");
    }

    #[test]
    fn a_filter_value_cannot_break_out_of_the_literal() {
        for bad in ["x' or '1' eq '1", "a; DROP", "a&b", "$top", "a'b", "a(b)", "a\\b", "a|b"] {
            assert!(filter_literal(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(filter_literal("").is_err());
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
                filter: None,
                time_window: w,
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
}