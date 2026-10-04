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
pub mod dates;
pub mod doctypes;
pub mod documents;
pub mod metadata;
pub mod planner;
pub mod query;
pub mod transport;

pub use ask::{Answer, AnswerRow};

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
}

/// Connects and reports what the live server exposes, so a report never assumes
/// a field the installation does not have.
pub async fn probe(c: &Credentials) -> Result<Probe, String> {
    let xml = transport::get_text(c, "$metadata").await?;
    Ok(check(&metadata::parse(&xml)))
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
                    Some(documents::Line { item_code, quantity, price })
                })
                .collect()
        })
        .unwrap_or_default();
    let doc = documents::Document { doc_type, card_code, doc_date, lines };
    documents::validate(&doc)?;
    let payload = documents::build_payload(&doc);
    transport::post(c, set, &payload).await
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
