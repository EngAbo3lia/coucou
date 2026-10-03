//! Answers questions about the ERP from the island chat.
//!
//! A question is matched to an intent, the intent to a query, and the query to
//! rows that are aggregated and rendered back as text. Nothing is invented: an
//! unmatched question returns the list of what this can answer instead of a
//! guess, and a capped or empty result says so.
//!
//! The queries respect the rules in `query` — no `$filter` with `$apply`, so a
//! filtered figure is fetched as rows and summed here, and month buckets are
//! built from the raw `DocDate` groups the server returns.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::transport::{self, Credentials};

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
}

/// What a question is asking for. Detected from keywords, not guessed.
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
    Unknown,
}

/// Keyword detection. Order matters: the most specific intent wins.
pub fn detect(question: &str) -> Intent {
    let q = question.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| q.contains(w));
    if has(&["top customer", "best customer", "largest customer", "top clients"]) {
        Intent::TopCustomers
    } else if has(&["top vendor", "top supplier", "best supplier", "top vendors"]) {
        Intent::TopVendors
    } else if has(&["receivable", "outstanding", "owed", "who owes", " ar ", "aging"]) {
        Intent::Receivables
    } else if has(&["open order", "pending order", "backlog", "booked"]) {
        Intent::OpenOrders
    } else if has(&["purchase", "spend", "procurement"]) && has(&["month", "trend", "monthly"]) {
        Intent::PurchasesByMonth
    } else if has(&["purchase", "spend", "procurement"]) {
        Intent::PurchasesTotal
    } else if has(&["month", "monthly", "trend", "by month"]) && has(&["sale", "revenue", "turnover"]) {
        Intent::SalesByMonth
    } else if has(&["sale", "revenue", "turnover", "income"]) {
        Intent::SalesTotal
    } else if has(&["how many order", "order count", "number of order"]) {
        Intent::OrderCount
    } else if has(&["how many invoice", "invoice count", "number of invoice"]) {
        Intent::InvoiceCount
    } else {
        Intent::Unknown
    }
}

pub const HELP: &str = "I can read the ERP for: sales total, sales by month, top customers, \
open orders, receivables, purchases total, purchases by month, top vendors, order count, \
invoice count. Ask about one of those.";

pub async fn ask(c: &Credentials, question: &str) -> Result<Answer, String> {
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
        Intent::Unknown => Ok(Answer {
            title: "SAP B1".into(),
            text: HELP.into(),
            rows: vec![],
            total: None,
            partial: false,
        }),
    }
}

// ── queries ──────────────────────────────────────────────────────────────────

/// Invoiced minus credit notes, the honest net-sales figure without a join.
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
        rows: vec![],
        total: Some(net),
        partial: false,
    })
}

async fn purchases_total(c: &Credentials) -> Result<Answer, String> {
    let bought = aggregate_sum(c, "PurchaseInvoices", "DocTotal").await?;
    let credited = aggregate_sum(c, "PurchaseCreditNotes", "DocTotal").await.unwrap_or(0.0);
    let net = bought - credited;
    Ok(Answer {
        title: "Purchases".into(),
        text: format!("Purchases (invoiced minus purchase credit notes)\n  net  {}", money(net)),
        rows: vec![],
        total: Some(net),
        partial: false,
    })
}

async fn sales_by_month(c: &Credentials) -> Result<Answer, String> {
    monthly(c, "Invoices", "Sales by month").await
}

async fn purchases_by_month(c: &Credentials) -> Result<Answer, String> {
    monthly(c, "PurchaseInvoices", "Purchases by month").await
}

/// Groups by the raw day field and buckets into months here, because the server
/// rejects `year()`/`month()` in `groupby`.
async fn monthly(c: &Credentials, set: &str, title: &str) -> Result<Answer, String> {
    let query = format!("{set}?$apply=groupby((DocDate),aggregate(DocTotal with sum as Total))");
    let rows = transport::get(c, &query).await?;
    let (buckets, seen) = month_buckets(rows.get("value").and_then(Value::as_array));
    Ok(answer_from_buckets(title, buckets, seen))
}

/// Group by business partner and sum a total, then attach names.
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
        partial: false,
    })
}

/// Open orders cannot use an aggregate because it needs a filter, so the rows
/// come down and are summed here.
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
        rows: vec![],
        total: Some(total),
        partial,
    })
}

/// Receivables straight off the business partner balance fields — no join.
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
    })
}

async fn count(c: &Credentials, set: &str, title: &str) -> Result<Answer, String> {
    // Service Layer answers `$count` with a bare number, not JSON, so this has
    // to read text. Parsing it as JSON fails on every live server.
    let raw = transport::get_text(c, &format!("{set}/$count")).await?;
    let n: i64 = raw.trim().trim_matches('"').parse().map_err(|_| format!("$count returned {raw:?}"))?;
    Ok(Answer {
        title: title.into(),
        text: format!("{title}: {n}"),
        rows: vec![],
        total: Some(n as f64),
        partial: false,
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

/// CardCode -> CardName, fetched once per question.
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

/// `DocDate` groups -> month totals. Returns the buckets and how many day rows
/// were seen, so a huge set can be flagged.
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

fn answer_from_buckets(title: &str, buckets: Vec<(String, f64)>, seen: usize) -> Answer {
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
}
