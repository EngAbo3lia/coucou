//! Turns a raw Service Layer write failure into an action the user — and the
//! agent — can take.
//!
//! Business One answers a rejected write with an opaque string: `-10 Quantity
//! falls into negative inventory [DocumentLines.ItemCode]`, `HTTP 400:
//! TaxExtension.BillToState`. Those are correct but useless in a chat bubble.
//! `classify` reads the string and the document that produced it, and names the
//! fix: receive stock, create the item, set the partner's state, add an exchange
//! rate. Each fix carries the question to ask next, so the chat can offer it.

use super::documents;

/// What a failed write actually needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Fix {
    /// The item is short: a goods receipt must run before this document.
    ReceiveStock { item: String },
    /// The item is managed by serial or batch numbers, so its line needs them.
    BatchSerial { item: String },
    /// The item does not exist yet.
    CreateItem { item: String },
    /// The partner has no billing state, which every document needs.
    PartnerState { card: String },
    /// The partner does not exist yet.
    CreatePartner { card: String },
    /// No exchange rate for the currency on that date.
    ExchangeRate { currency: String },
    /// A sales order needs a due date.
    DueDate,
    /// A field the server demanded is missing.
    MissingField { field: String },
    /// Nothing recognised: the server's own words, so nothing is hidden.
    Unknown(String),
}

/// The first line item of the document, which is the one most errors name.
fn first_item(doc: &documents::Document) -> String {
    doc.lines
        .first()
        .map(|l| l.item_code.clone())
        .unwrap_or_default()
}

/// The currency from the server's `Update the exchange rate , 'USD'` message.
fn quoted_currency(error: &str) -> Option<String> {
    let start = error.find('\'')?;
    let rest = &error[start + 1..];
    let end = rest.find('\'')?;
    let code = rest[..end].trim();
    (!code.is_empty()).then(|| code.to_string())
}

/// The `[TABLE.FIELD]` the server named, e.g. `[OITM.ItemCode]` -> `ItemCode`.
fn bracketed_field(error: &str) -> Option<String> {
    let start = error.find('[')?;
    let rest = &error[start + 1..];
    let end = rest.find(']')?;
    let inner = &rest[..end];
    let field = inner.split('.').next_back()?.trim();
    (!field.is_empty() && !inner.contains("line")).then(|| field.to_string())
}

pub fn classify(doc: &documents::Document, error: &str) -> Fix {
    classify_parts(&doc.card_code, &first_item(doc), error)
}

/// Classify from the two identifiers the fixes name, so master-data and
/// inventory writes (which have no document) use the same rules.
pub fn classify_parts(card: &str, item: &str, error: &str) -> Fix {
    let lowered = error.to_lowercase();

    // Order matters: the most specific phrase first.
    if lowered.contains("batch/serial") || lowered.contains("batch or serial") {
        return Fix::BatchSerial { item: item.to_string() };
    }
    if lowered.contains("negative inventory") {
        return Fix::ReceiveStock { item: item.to_string() };
    }
    if lowered.contains("exchange rate") {
        return Fix::ExchangeRate {
            currency: quoted_currency(error).unwrap_or_else(|| "the document currency".into()),
        };
    }
    if lowered.contains("billing state") || error.contains("TaxExtension.BillToState") {
        return Fix::PartnerState { card: card.to_string() };
    }
    if lowered.contains("enter due date") || error.contains("DocDueDate") {
        return Fix::DueDate;
    }
    if lowered.contains("item not found") || lowered.contains("item number is missing") {
        return Fix::CreateItem { item: item.to_string() };
    }
    if lowered.contains("customer record not found") || lowered.contains("business partner") {
        return Fix::CreatePartner { card: card.to_string() };
    }
    if let Some(field) = bracketed_field(error) {
        return Fix::MissingField { field };
    }
    Fix::Unknown(error.trim().to_string())
}

impl Fix {
    /// What to tell the user.
    pub fn message(&self) -> String {
        match self {
            Fix::ReceiveStock { item } => format!(
                "{item} has no stock, so Business One blocks the document. Receive stock for it first."
            ),
            Fix::BatchSerial { item } => format!(
                "{item} is tracked by batch or serial numbers, so its line needs them before it can be posted."
            ),
            Fix::CreateItem { item } => format!(
                "{item} is not an item in this company yet. Create it first."
            ),
            Fix::PartnerState { card } => format!(
                "{card} has no billing state, so Business One rejects every document for it. Set a bill-to state first."
            ),
            Fix::CreatePartner { card } => format!(
                "{card} is not a business partner in this company yet. Create it first."
            ),
            Fix::ExchangeRate { currency } => format!(
                "There is no exchange rate for {currency} on that date. Pick a date that has one, or add the rate."
            ),
            Fix::DueDate => "A sales order needs a due date. Give one and post again.".into(),
            Fix::MissingField { field } => format!("The server needs {field} before it can accept this document."),
            Fix::Unknown(raw) => raw.clone(),
        }
    }

    /// The question the chat can offer as a next step, when there is one.
    pub fn suggestion(&self) -> Option<String> {
        match self {
            Fix::ReceiveStock { item } => Some(format!("receive 1000 {item}")),
            Fix::CreateItem { item } => Some(format!("create item {item}")),
            Fix::CreatePartner { card } => Some(format!("create customer {card}")),
            Fix::PartnerState { card } => Some(format!("set the billing state on {card}")),
            _ => None,
        }
    }
}

/// The message to surface for a failed write.
pub fn explain(doc: &documents::Document, error: &str) -> String {
    classify(doc, error).message()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sapb1::documents::{self, Line};

    fn doc(card: &str, item: &str) -> documents::Document {
        documents::Document {
            doc_type: documents::find_by_kind("ar_invoice").unwrap(),
            card_code: card.into(),
            doc_date: Some("2026-08-15".into()),
            due_date: Some("2026-09-14".into()),
            lines: vec![Line { item_code: item.into(), quantity: 1.0, price: None, base_line: None }],
            base: None,
        }
    }

    #[test]
    fn negative_inventory_names_the_item_and_the_fix() {
        let fix = classify(&doc("C0001", "A00001"), "-10: Quantity falls into negative inventory  [DocumentLines.ItemCode][line: 1]");
        assert_eq!(fix, Fix::ReceiveStock { item: "A00001".into() });
        assert!(fix.message().contains("no stock"));
        assert_eq!(fix.suggestion().as_deref(), Some("receive 1000 A00001"));
    }

    #[test]
    fn batch_serial_is_recognised() {
        let fix = classify(&doc("C0001", "B10000"), "-4014: Cannot add row without complete selection of batch/serial numbers");
        assert_eq!(fix, Fix::BatchSerial { item: "B10000".into() });
        assert!(fix.suggestion().is_none());
    }

    #[test]
    fn a_missing_partner_offers_to_create_it() {
        let fix = classify(&doc("ZZZ", "A00001"), "-2028: Customer record not found");
        assert_eq!(fix, Fix::CreatePartner { card: "ZZZ".into() });
        assert_eq!(fix.suggestion().as_deref(), Some("create customer ZZZ"));
    }

    #[test]
    fn the_exchange_rate_currency_is_read_back() {
        let fix = classify(&doc("C70000", "A00001"), "HTTP 400: Update the exchange rate  , 'USD'");
        assert_eq!(fix, Fix::ExchangeRate { currency: "USD".into() });
    }

    #[test]
    fn the_billing_state_error_names_the_partner() {
        let fix = classify(&doc("C20000", "A00001"), "HTTP 400: Invalid value [TaxExtension.BillToState][line: 0]");
        assert_eq!(fix, Fix::PartnerState { card: "C20000".into() });
        assert!(fix.message().contains("billing state"));
    }

    #[test]
    fn an_unrecognised_error_is_passed_through_untouched() {
        let fix = classify(&doc("C0001", "A00001"), "-9999: something new");
        assert_eq!(fix, Fix::Unknown("-9999: something new".into()));
        assert_eq!(fix.message(), "-9999: something new");
    }
}
