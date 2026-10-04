//! SAP Business One Service Layer.
//!
//! Everything Coucou knows about a Business One installation lives in this
//! module, as code: the entity catalogue (`catalogue`, generated from a live
//! `$metadata`), the runtime probe (`metadata`), the document types and the
//! chain between them (`doctypes`), and what the API will and will not do
//! (`query`).
//!
//! Verified against a live server, Business One 10.0 FP 3400:
//!
//! | Fact | Value |
//! |---|---|
//! | Endpoint | `https://<host>:50000/b1s/v1` (OData v3) and `/b1s/v2` (v4) |
//! | Login | `POST /Login` `{CompanyDB, UserName, Password}` -> `SessionId` |
//! | Session | 30-minute idle timeout, reuse it; the first call costs ~5 s |
//! | TLS | self-signed certificate, invalid certs must be accepted |
//! | Metadata | `/b1s/v2/$metadata`, 2.1 MB, 460 entity sets |
//! | Data | documents are small here: 1 259 orders, 1 406 invoices, 1 150 receipts |
//!
//! Entity sets share types — `Orders`, `Invoices`, `CreditNotes` and the
//! purchase documents are all `SAPB1.Document`; `IncomingPayments` and
//! `VendorPayments` are both `SAPB1.Payment`. The entity set is what selects the
//! document kind, never the type.

pub mod ask;
pub mod catalogue;
mod coverage;
pub mod dates;
pub mod doctypes;
pub mod documents;
pub mod entities;
pub mod metadata;
pub mod planner;
pub mod query;
pub mod recovery;
pub mod transport;

pub use ask::Answer;

use serde::Serialize;
use serde_json::Value;

use transport::Credentials;

/// The report entity sets every probe checks for.
pub const REPORT_SETS: &[&str] = &[
    "Orders",
    "Invoices",
    "CreditNotes",
    "DeliveryNotes",
    "IncomingPayments",
    "PurchaseOrders",
    "PurchaseInvoices",
    "VendorPayments",
    "BusinessPartners",
    "Items",
];

/// One entity set, checked against the live server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetProbe {
    pub entity_set: String,
    pub present: bool,
    pub entity_type: Option<String>,
    pub field_count: usize,
    /// Report fields the server does not have. Empty is the good case.
    pub missing_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub entity_set_count: usize,
    pub type_count: usize,
    pub sets: Vec<SetProbe>,
    /// True when every report set and every report field exists live.
    pub ready: bool,
    /// The Business One version the committed schema catalogue was built from.
    pub schema_version: String,
    /// The version the server reported at login. Empty when it could not be read.
    pub server_version: String,
    /// True when the catalogue matches the server's version. False means
    /// Business One was upgraded and the schema snapshot needs regenerating.
    pub schema_current: bool,
}

/// Connects and reports what the live server exposes, so a report never assumes
/// a field the installation does not have.
pub async fn probe(c: &Credentials) -> Result<Probe, String> {
    let xml = transport::get_text(c, "$metadata").await?;
    let mut probe = check(&metadata::parse(&xml));
    probe.schema_version = catalogue::SAP_VERSION.to_string();
    // The login response always carries the version. A mismatch against the one
    // the catalogue was generated from means the snapshot is stale; an unreadable
    // version is left as "current" so a hiccup never raises a false alarm.
    if let Ok(server) = transport::login_version(c).await {
        probe.schema_current = server == catalogue::SAP_VERSION;
        probe.server_version = server;
    }
    Ok(probe)
}

/// Service Layer credentials from the Credential Manager. The password never
/// crosses the IPC boundary and never touches disk.
pub fn credentials_from_secrets() -> Result<Credentials, String> {
    let need = |key: &str, what: &str| {
        crate::secrets::get(key).ok_or_else(|| format!("{what} is not configured"))
    };
    Ok(Credentials {
        base_url: need("sapb1-url", "The Service Layer URL")?,
        company: need("sapb1-company", "The company database")?,
        user: need("sapb1-user", "The Business One user")?,
        password: need("sapb1-password", "The Business One password")?,
        version: 1,
    })
}

/// Creates a sales or purchase document. Called by the app only after the user
/// confirms the preview shown in the chat. `spec` is the shape the planner
/// carried in the `payload` field: `{set, cardCode, docDate, lines[]}`.
pub async fn create_document(c: &Credentials, spec: &Value) -> Result<Value, String> {
    let set = spec
        .get("set")
        .and_then(Value::as_str)
        .ok_or("No document set.")?;
    let doc_type = documents::find_by_set(set)
        .ok_or_else(|| format!("{set} is not a document type I can create."))?;
    let card_code = spec.get("cardCode").and_then(Value::as_str).unwrap_or("").to_string();
    let doc_date = spec.get("docDate").and_then(Value::as_str).map(|s| s.to_string());
    let lines: Vec<documents::Line> = spec
        .get("lines")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let item_code = l.get("itemCode").and_then(Value::as_str).unwrap_or("").to_string();
                    if item_code.is_empty() {
                        return None;
                    }
                    let quantity = l.get("quantity").and_then(Value::as_f64).unwrap_or(0.0);
                    let price = l.get("price").and_then(Value::as_f64);
                    let base_line = l.get("baseLine").and_then(Value::as_i64);
                    Some(documents::Line { item_code, quantity, price, base_line })
                })
                .collect()
        })
        .unwrap_or_default();
    // Business One derives the tax extension from the partner, so nothing is
    // sent for it: supplying one by hand is rejected or silently wrong.
    //
    // What it will not do is guess a missing state. A partner with no billing
    // state makes every document for that partner fail with an opaque
    // `TaxExtension.BillToState` error, so it is caught here, before the write
    // and before the user is asked to confirm.
    if let Some(reason) = partner_state_problem(c, doc_type, &card_code).await {
        return Err(reason);
    }
    // A copy is taken from the document the planner named, so Business One derives
    // price, tax and currency from the source instead of trusting a rebuild.
    let base = match (spec.get("baseEntry").and_then(Value::as_i64), spec.get("baseType").and_then(Value::as_i64)) {
        (Some(entry), Some(base_type)) if entry > 0 => Some(documents::BaseDocument { entry, base_type }),
        _ => None,
    };
    let due_date = doc_type.needs_due_date.then(|| due_date_for(&doc_date));
    let doc = documents::Document { doc_type, card_code, doc_date, due_date, lines, base };
    documents::validate(&doc)?;
    let payload = documents::build_payload(&doc);
    // A rejected write is turned into the action it needs — receive stock, set a
    // billing state, add a rate — instead of the server's opaque string.
    transport::post(c, set, &payload)
        .await
        .map_err(|e| recovery::explain(&doc, &e))
}

/// Creates a master-data or inventory entity — an item, a business partner, a
/// goods receipt — from the field values the confirm dialog collected. Called by
/// the app only after the user confirms; nothing here writes silently.
pub async fn write_entity(
    c: &Credentials,
    set: &str,
    values: &Value,
    lines: &Value,
) -> Result<Value, String> {
    let entity = entities::find(set)
        .ok_or_else(|| format!("{set} is not an entity I can create."))?;
    let values = values.as_object().cloned().unwrap_or_default();

    let missing = entities::required_missing(entity, &values);
    if !missing.is_empty() {
        return Err(format!("Fill in {} before posting.", missing.join(", ")));
    }

    let lines: Vec<documents::Line> = lines
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let item_code = l.get("itemCode").and_then(Value::as_str).unwrap_or("").to_string();
                    (!item_code.is_empty()).then(|| documents::Line {
                        item_code,
                        quantity: l.get("quantity").and_then(Value::as_f64).unwrap_or(0.0),
                        price: l.get("price").and_then(Value::as_f64),
                        base_line: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if entity.needs_lines && lines.is_empty() {
        return Err(format!("An {} needs at least one line.", entity.name));
    }

    let payload = entities::build_payload(entity, &values, &lines)?;
    let card = entities::value_for(&values, "CardCode").unwrap_or("").to_string();
    let item = lines.first().map(|l| l.item_code.clone()).unwrap_or_default();
    transport::post(c, set, &payload)
        .await
        .map_err(|e| recovery::classify_parts(&card, &item, &e).message())
}

/// Sales documents this company expects to carry a due date. A purchase order
/// asks its vendor for payment terms, so the date is optional there.
fn due_date_for(doc_date: &Option<String>) -> String {
    match doc_date {
        Some(d) if d.len() == 10 => {
            // Thirty days out, without pulling in a date library: shift the day
            // and let the caller post an explicit date when the user wants one.
            let (y, m, day) = (
                d[0..4].parse::<i32>().unwrap_or(2015),
                d[5..7].parse::<u32>().unwrap_or(1),
                d[8..10].parse::<u32>().unwrap_or(1),
            );
            let last = days_in_month(y, m);
            if day + 30 <= last {
                format!("{y:04}-{m:02}-{:02}", day + 30)
            } else {
                let next = if m == 12 { 1 } else { m + 1 };
                let ny = if m == 12 { y + 1 } else { y };
                format!("{ny:04}-{next:02}-{:02}", day + 30 - last)
            }
        }
        _ => {
            let (y, m, d) = dates::today_ymd();
            format!("{y:04}-{m:02}-{d:02}")
        }
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Why this partner cannot be used for this document, or `None` when it can.
///
/// Only sales documents are checked: a purchase document has no billing state
/// requirement, and an inventory document has no partner at all.
async fn partner_state_problem(
    c: &Credentials,
    doc_type: &'static documents::DocumentType,
    card_code: &str,
) -> Option<String> {
    if doc_type.cycle != "sales" {
        return None;
    }
    let url = format!(
        "BusinessPartners?$filter=CardCode eq {}&$select=CardCode,CardName,BillToState&$top=1",
        query::filter_literal(card_code).ok()?
    );
    let found = transport::get(c, &url).await.ok()?;
    let row = found.get("value")?.as_array()?.first()?;
    // An unreadable partner is the server's problem to report, not a guess here.
    let name = row.get("CardName").and_then(Value::as_str).unwrap_or(card_code);
    match row.get("BillToState").and_then(Value::as_str) {
        Some(state) if !state.trim().is_empty() => None,
        _ => Some(format!(
            "{name} ({card_code}) has no billing state, so Business One will reject this \
             {}. Set a bill-to state on the customer first.",
            doc_type.name
        )),
    }
}

fn check(md: &metadata::Metadata) -> Probe {
    let sets = REPORT_SETS
        .iter()
        .map(|set| set_probe(md, set))
        .collect::<Vec<_>>();
    Probe {
        entity_set_count: md.entity_sets.len(),
        type_count: md.types.len(),
        ready: sets.iter().all(|s| s.present && s.missing_fields.is_empty()),
        sets,
        schema_version: catalogue::SAP_VERSION.to_string(),
        server_version: String::new(),
        schema_current: true,
    }
}

fn set_probe(md: &metadata::Metadata, set: &str) -> SetProbe {
    let found = md.entity_set(set);
    let fields: &[&str] = query::report_fields(set).unwrap_or(&[]);
    let missing_fields = fields
        .iter()
        .filter(|f| !md.has_field(set, f))
        .map(|f| (*f).to_string())
        .collect();
    SetProbe {
        entity_set: set.to_string(),
        present: found.is_some(),
        entity_type: found.map(|s| s.entity_type.clone()),
        field_count: found
            .and_then(|s| md.entity_type(&s.entity_type))
            .map(|t| t.properties.len())
            .unwrap_or(0),
        missing_fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_catalogue_covers_every_report_set() {
        for set in REPORT_SETS {
            assert!(catalogue::entity_set(set).is_some(), "catalogue lacks {set}");
            assert!(query::report_fields(set).is_some(), "no report fields for {set}");
        }
    }

    #[test]
    fn offline_catalogue_knows_report_fields() {
        for (set, fields) in query::REPORT_FIELDS {
            // Count-only sets have no report fields to check.
            if fields.is_empty() {
                continue;
            }
            let ty = catalogue::type_for_set(set).expect("type for set");
            for field in *fields {
                assert!(
                    ty.properties.iter().any(|p| p.name == *field),
                    "{set} lacks {field} in the catalogue"
                );
            }
        }
    }

    /// Hits a real server. Run with credentials in the environment:
    /// `SAPB1_URL=https://host:50000 SAPB1_COMPANY=DB SAPB1_USER=manager
    ///  SAPB1_PASSWORD=… cargo test --release --lib -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a live Service Layer"]
    fn live_probe_finds_every_report_set() {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let (Some(url), Some(company), Some(user), Some(password)) = (
            var("SAPB1_URL"),
            var("SAPB1_COMPANY"),
            var("SAPB1_USER"),
            var("SAPB1_PASSWORD"),
        ) else {
            panic!("set SAPB1_URL, SAPB1_COMPANY, SAPB1_USER, SAPB1_PASSWORD");
        };
        let credentials = Credentials { base_url: url, company, user, password, version: 1 };
        let probe = tauri::async_runtime::block_on(probe(&credentials)).expect("probe");
        println!("{probe:#?}");
        assert!(probe.ready, "not ready: {:?}", probe.sets);
    }

    /// Drives the real planner path against a live server and the configured chat
    /// backend, so a two-turn clarification is exercised end to end. The model is
    /// non-deterministic, so this asserts the shape (a valid kind), not exact text.
    ///
    /// Reads the same Credential Manager entries the app uses:
    /// `cargo test --release --lib live_ask -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a live Service Layer and a configured chat backend"]
    fn live_ask_answers_every_intent() {
        let c = credentials_from_secrets().expect("SAP credentials in the Credential Manager");
        let settings = crate::settings::load();

        let questions = [
            "hi",
            "what are our sales",
            "how many invoices do we have",
            "how many employees",
            "sales by month last year",
            "sales in 2015",
            "net sales in 2015",
            "sales from March to June 2015",
            "show me employees",
            "list some items",
            "show me customer details",
        ];
        for q in questions {
            let answer = tauri::async_runtime::block_on(ask::ask(&c, &settings, q, &[]))
                .unwrap_or_else(|e| panic!("{q:?} failed: {e}"));
            println!("--- {q} ---\nkind={} plan={:?}\n{}", answer.kind, answer.plan, answer.text);
            assert!(
                matches!(answer.kind.as_str(), "result" | "answer" | "clarify"),
                "{q:?} returned an unknown kind {:?}",
                answer.kind
            );
        }
    }

    /// Drives the write path through the real planner: a create and a copy both
    /// build a preview and never post. The model is non-deterministic, so this
    /// asserts the shape (a confirm preview or a clarification), not the codes.
    #[test]
    #[ignore = "needs a live Service Layer and a configured chat backend"]
    fn live_write_path_previews() {
        let c = credentials_from_secrets().expect("SAP credentials");
        let settings = crate::settings::load();
        let questions = [
            "create a sales order for customer C0001 with 2 units of item A00001",
            "copy sales order 1 to an invoice",
        ];
        for q in questions {
            let answer = tauri::async_runtime::block_on(ask::ask(&c, &settings, q, &[]))
                .unwrap_or_else(|e| panic!("{q:?} failed: {e}"));
            println!("--- {q} ---\nkind={} title={}\n{}\npayload={:?}", answer.kind, answer.title, answer.text, answer.payload.is_some());
            assert!(
                matches!(answer.kind.as_str(), "confirm" | "clarify" | "result"),
                "{q:?} returned an unexpected kind {:?}",
                answer.kind
            );
        }
    }

    /// Two turns: the model must use the first answer to resolve the second turn
    /// instead of re-asking. This is the awareness the chat needs.
    #[test]
    #[ignore = "needs a live Service Layer and a configured chat backend"]
    fn live_conversation_resolves_a_clarification() {
        let c = credentials_from_secrets().expect("SAP credentials");
        let settings = crate::settings::load();

        let first = tauri::async_runtime::block_on(ask::ask(&c, &settings, "give me sales", &[]))
            .expect("first turn");
        println!("first: kind={} {}", first.kind, first.text);

        let history = vec![
            planner::ChatTurn { role: "user".into(), content: "give me sales".into() },
            planner::ChatTurn { role: "assistant".into(), content: first.text.clone() },
        ];
        let second = tauri::async_runtime::block_on(ask::ask(&c, &settings, "last month", &history))
            .expect("second turn");
        println!("second: kind={} {}", second.kind, second.text);
        assert!(
            second.kind != "clarify",
            "with the history the model should not re-ask: {}",
            second.text
        );
    }
}
