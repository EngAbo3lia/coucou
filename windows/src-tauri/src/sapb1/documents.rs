//! SAP document types and payload building for the create path.
//!
//! Sales and purchase cycles mirror each other: each step in one has a twin in
//! the other. This registry names the Service Layer entity set for each, so the
//! executor posts to the right endpoint and the preview reads as a human label.

use serde_json::{json, Value};

/// One document type the assistant can create.
#[derive(Debug, Clone)]
pub struct DocumentType {
    /// Stable id used by the planner (`"sales_order"`, `"ar_invoice"`, …).
    pub kind: &'static str,
    /// Service Layer entity set to `POST` to (`"Orders"`, `"Invoices"`, …).
    pub set: &'static str,
    /// Human name for the preview.
    pub name: &'static str,
    /// `"sales"` or `"purchase"`.
    pub cycle: &'static str,
    /// Payment terms. Required on a sales order by every company verified
    /// against: the server answers `Enter due date` when it is missing.
    pub needs_due_date: bool,
}

pub const DOCUMENT_TYPES: &[DocumentType] = &[
    // Sales cycle
    DocumentType { kind: "sales_quotation", set: "Quotations", name: "Sales quotation", cycle: "sales", needs_due_date: true },
    DocumentType { kind: "sales_order", set: "Orders", name: "Sales order", cycle: "sales", needs_due_date: true },
    DocumentType { kind: "delivery_note", set: "DeliveryNotes", name: "Delivery note", cycle: "sales", needs_due_date: false },
    DocumentType { kind: "ar_invoice", set: "Invoices", name: "A/R invoice", cycle: "sales", needs_due_date: false },
    DocumentType { kind: "ar_credit_memo", set: "CreditNotes", name: "A/R credit memo", cycle: "sales", needs_due_date: false },
    DocumentType { kind: "sales_return", set: "Returns", name: "Sales return", cycle: "sales", needs_due_date: false },
    // Purchase cycle
    DocumentType { kind: "purchase_quotation", set: "PurchaseQuotations", name: "Purchase quotation", cycle: "purchase", needs_due_date: true },
    DocumentType { kind: "purchase_order", set: "PurchaseOrders", name: "Purchase order", cycle: "purchase", needs_due_date: true },
    DocumentType { kind: "goods_receipt", set: "PurchaseDeliveryNotes", name: "Goods receipt", cycle: "purchase", needs_due_date: false },
    DocumentType { kind: "ap_invoice", set: "PurchaseInvoices", name: "A/P invoice", cycle: "purchase", needs_due_date: true },
    DocumentType { kind: "ap_credit_memo", set: "PurchaseCreditNotes", name: "A/P credit memo", cycle: "purchase", needs_due_date: false },
    DocumentType { kind: "purchase_return", set: "PurchaseReturns", name: "Purchase return", cycle: "purchase", needs_due_date: false },
];

pub fn find_by_kind(kind: &str) -> Option<&'static DocumentType> {
    DOCUMENT_TYPES.iter().find(|d| d.kind == kind)
}

pub fn find_by_set(set: &str) -> Option<&'static DocumentType> {
    DOCUMENT_TYPES.iter().find(|d| d.set == set)
}

/// The document a source is copied into: a sales order becomes an A/R invoice,
/// a purchase order an A/P invoice. Only these two "copy to" links exist.
pub fn copy_target(source_set: &str) -> Option<&'static DocumentType> {
    match source_set {
        "Orders" => find_by_kind("ar_invoice"),
        "PurchaseOrders" => find_by_kind("ap_invoice"),
        _ => None,
    }
}

/// One line the assistant proposes.
#[derive(Debug, Clone)]
pub struct Line {
    pub item_code: String,
    pub quantity: f64,
    pub price: Option<f64>,
    /// The line number this line copies from, set only on a copy.
    pub base_line: Option<i64>,
    /// Serial numbers for a serial-managed item: one per unit.
    pub serials: Vec<String>,
    /// A batch number for a batch-managed item; the whole line quantity is
    /// allocated to it unless the user splits it later.
    pub batch: Option<String>,
}

/// A document the assistant wants to create, before it is posted.
#[derive(Debug, Clone)]
pub struct Document {
    pub doc_type: &'static DocumentType,
    pub card_code: String,
    pub doc_date: Option<String>,
    /// Payment due date. Required on a sales order by the companies verified
    /// against; `None` leaves it to the server default.
    pub due_date: Option<String>,
    pub lines: Vec<Line>,
    /// Set when the document is copied from another one. Business One then
    /// derives price, tax and currency from the base document instead of
    /// trusting values rebuilt by hand, which is what makes a copy correct.
    pub base: Option<BaseDocument>,
}

/// The document a copy is taken from.
#[derive(Debug, Clone)]
pub struct BaseDocument {
    pub entry: i64,
    /// `17` for a sales order, `540` for a purchase order.
    pub base_type: i64,
}

/// The `BaseType` code Business One uses for each copyable source.
pub fn base_type_for(source_set: &str) -> Option<i64> {
    match source_set {
        "Orders" => Some(17),
        "PurchaseOrders" => Some(540),
        _ => None,
    }
}

/// Builds the Service Layer `POST` body for a document.
pub fn build_payload(doc: &Document) -> Value {
    let doc_lines: Vec<Value> = doc
        .lines
        .iter()
        .map(|l| {
            let mut obj = serde_json::Map::new();
            obj.insert("ItemCode".into(), json!(l.item_code));
            obj.insert("Quantity".into(), json!(l.quantity));
            if let Some(p) = l.price {
                obj.insert("UnitPrice".into(), json!(p));
            }
            if let Some(base) = &doc.base {
                // `LineNum` is the base document's own line number, which is
                // what links the copied line back to its source.
                obj.insert("BaseEntry".into(), json!(base.entry));
                obj.insert("BaseLine".into(), json!(l.base_line.unwrap_or(0)));
                obj.insert("BaseType".into(), json!(base.base_type));
            }
            // A batch-managed item rejects the line without its numbers
            // (`-4014 Cannot add row without complete selection`), so the whole
            // quantity is allocated to the batch the user gave.
            if let Some(batch) = l.batch.as_ref().filter(|b| !b.trim().is_empty()) {
                obj.insert(
                    "BatchNumbers".into(),
                    json!([{ "BatchNumber": batch.trim(), "Quantity": l.quantity }]),
                );
            }
            let serials: Vec<Value> = l
                .serials
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| json!({ "SerialNumber": s, "Quantity": 1.0 }))
                .collect();
            if !serials.is_empty() {
                obj.insert("SerialNumbers".into(), json!(serials));
            }
            Value::Object(obj)
        })
        .collect();
    let mut body = serde_json::Map::new();
    body.insert("CardCode".into(), json!(doc.card_code));
    if let Some(d) = &doc.doc_date {
        body.insert("DocDate".into(), json!(d));
    }
    if let Some(d) = &doc.due_date {
        body.insert("DocDueDate".into(), json!(d));
    }
    body.insert("DocumentLines".into(), json!(doc_lines));
    Value::Object(body)
}

/// Validates a proposed document before it is shown or posted. Checks what is
/// knowable offline (empty card, empty lines, bad quantities, unknown type).
pub fn validate(doc: &Document) -> Result<(), String> {
    if doc.card_code.trim().is_empty() {
        return Err("No customer or supplier selected.".into());
    }
    if doc.lines.is_empty() {
        return Err("No lines to post.".into());
    }
    for l in &doc.lines {
        if l.item_code.trim().is_empty() {
            return Err("A line has no item code.".into());
        }
        if !(l.quantity > 0.0) {
            return Err(format!("Quantity for {} must be positive.", l.item_code));
        }
    }
    Ok(())
}

/// A human-readable preview of a document, for the confirmation bubble.
pub fn preview_text(doc: &Document) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} — {}", doc.doc_type.name, doc.card_code));
    if let Some(d) = &doc.doc_date {
        out.push_str(&format!(" ({d})"));
    }
    out.push('\n');
    for l in &doc.lines {
        let price = l.price.map(|p| format!(" @ {p}")).unwrap_or_default();
        out.push_str(&format!("  {} × {}{}\n", l.item_code, l.quantity, price));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        Document {
            doc_type: find_by_kind("sales_order").unwrap(),
            card_code: "C0001".into(),
            doc_date: Some("2026-10-04".into()),
            due_date: Some("2026-11-03".into()),
            lines: vec![Line { item_code: "A00001".into(), quantity: 2.0, price: Some(100.0), base_line: None, serials: Vec::new(), batch: None }],
            base: None,
        }
    }

    #[test]
    fn every_sales_and_purchase_type_is_registered() {
        for kind in ["sales_quotation", "sales_order", "delivery_note", "ar_invoice",
            "ar_credit_memo", "sales_return", "purchase_quotation", "purchase_order",
            "goods_receipt", "ap_invoice", "ap_credit_memo", "purchase_return"] {
            assert!(find_by_kind(kind).is_some(), "{kind} is missing");
        }
        assert_eq!(find_by_set("Orders").unwrap().cycle, "sales");
        assert_eq!(find_by_set("PurchaseInvoices").unwrap().cycle, "purchase");
    }

    #[test]
    fn copy_target_maps_sales_and_purchase() {
        assert_eq!(copy_target("Orders").unwrap().set, "Invoices");
        assert_eq!(copy_target("PurchaseOrders").unwrap().set, "PurchaseInvoices");
        assert!(copy_target("Invoices").is_none());
    }

    #[test]
    fn payload_has_card_and_lines() {
        let v = build_payload(&doc());
        assert_eq!(v["CardCode"], json!("C0001"));
        assert_eq!(v["DocDate"], json!("2026-10-04"));
        assert_eq!(v["DocumentLines"][0]["ItemCode"], json!("A00001"));
        assert_eq!(v["DocumentLines"][0]["Quantity"], json!(2.0));
        assert_eq!(v["DocumentLines"][0]["UnitPrice"], json!(100.0));
    }

    #[test]
    fn payload_never_carries_a_hand_written_tax_extension() {
        // Business One derives tax from the partner; a hand-written extension is
        // either rejected or, worse, silently wrong.
        let v = build_payload(&doc());
        assert!(v.get("TaxExtension").is_none(), "payload invented TaxExtension: {v}");
        assert!(v.get("ShipToState").is_none(), "payload invented ShipToState: {v}");
    }

    #[test]
    fn payload_carries_the_due_date_only_when_given() {
        let v = build_payload(&doc());
        assert_eq!(v["DocDueDate"], json!("2026-11-03"));
        let mut d = doc();
        d.due_date = None;
        assert!(build_payload(&d).get("DocDueDate").is_none());
    }

    #[test]
    fn copy_payload_links_every_line_to_its_base_line() {
        let mut d = doc();
        d.base = Some(BaseDocument { entry: 1260, base_type: 17 });
        d.lines = vec![
            Line { item_code: "A00001".into(), quantity: 2.0, price: None, base_line: Some(0), serials: Vec::new(), batch: None },
            Line { item_code: "A00002".into(), quantity: 1.0, price: None, base_line: Some(1), serials: Vec::new(), batch: None },
        ];
        let v = build_payload(&d);
        for (i, line) in v["DocumentLines"].as_array().unwrap().iter().enumerate() {
            assert_eq!(line["BaseEntry"], json!(1260), "line {i} lost the base entry");
            assert_eq!(line["BaseLine"], json!(i as i64), "line {i} has the wrong base line");
            assert_eq!(line["BaseType"], json!(17), "line {i} has the wrong base type");
        }
    }

    #[test]
    fn a_batch_line_allocates_its_quantity_to_the_batch() {
        let mut d = doc();
        d.lines = vec![Line {
            item_code: "B10000".into(),
            quantity: 10.0,
            price: None,
            base_line: None,
            serials: Vec::new(),
            batch: Some("LOT-1".into()),
        }];
        let v = build_payload(&d);
        assert_eq!(v["DocumentLines"][0]["BatchNumbers"][0]["BatchNumber"], "LOT-1");
        assert_eq!(v["DocumentLines"][0]["BatchNumbers"][0]["Quantity"], 10.0);
        assert!(v["DocumentLines"][0].get("SerialNumbers").is_none());
    }

    #[test]
    fn a_serial_line_emits_one_entry_per_unit() {
        let mut d = doc();
        d.lines = vec![Line {
            item_code: "S1".into(),
            quantity: 2.0,
            price: None,
            base_line: None,
            serials: vec!["SN-1".into(), "SN-2".into()],
            batch: None,
        }];
        let v = build_payload(&d);
        let serials = v["DocumentLines"][0]["SerialNumbers"].as_array().unwrap();
        assert_eq!(serials.len(), 2);
        assert_eq!(serials[0]["SerialNumber"], "SN-1");
        assert_eq!(serials[1]["SerialNumber"], "SN-2");
    }

    #[test]
    fn only_orders_and_purchase_orders_name_a_base_type() {
        assert_eq!(base_type_for("Orders"), Some(17));
        assert_eq!(base_type_for("PurchaseOrders"), Some(540));
        assert_eq!(base_type_for("Invoices"), None);
    }

    #[test]
    fn validate_rejects_empty_or_bad_documents() {
        assert!(validate(&doc()).is_ok());
        let mut d = doc();
        d.card_code = "".into();
        assert!(validate(&d).is_err());
        d = doc();
        d.lines.clear();
        assert!(validate(&d).is_err());
        d = doc();
        d.lines[0].quantity = 0.0;
        assert!(validate(&d).is_err());
    }

    #[test]
    fn preview_reads_human() {
        let p = preview_text(&doc());
        assert!(p.contains("Sales order"));
        assert!(p.contains("C0001"));
        assert!(p.contains("A00001"));
    }
}

