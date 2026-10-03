//! Business One document types, and the chain that links a document to its
//! source.
//!
//! `DocumentLine.BaseType` is an `Edm.Int32`, not the metadata enum, so it
//! carries the raw DI API `BoObjectTypes` value. That is the only document-to-
//! document link the Service Layer exposes:
//!
//! ```text
//!   Order (17) -> DeliveryNote (15) -> Invoice (13)
//!   Order (17) -> Invoice (13)               (invoice straight off an order)
//!   Invoice (13) -> CreditNote (14)          (a return against an invoice)
//!   IncomingPayment.PaymentInvoices[] -> invoice DocEntry + SumApplied
//! ```
//!
//! Read a line's `BaseType` to learn what it came from and `BaseEntry` to learn
//! which document, then join in Rust. There is no OData navigation for these
//! links, so `$expand` and `$crossjoin` cannot follow them — see `query.rs`.

/// A document family and the entity set that serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocType {
    pub code: i32,
    pub name: &'static str,
    pub entity_set: &'static str,
    /// True once confirmed against a live server rather than the DI API enum.
    pub verified: bool,
}

/// Document families a report aggregates. Codes are the DI API `BoObjectTypes`
/// values; only the ones marked `verified` have been observed live.
pub const DOC_TYPES: &[DocType] = &[
    DocType { code: 13, name: "oInvoices", entity_set: "Invoices", verified: false },
    DocType { code: 14, name: "oCreditNotes", entity_set: "CreditNotes", verified: false },
    DocType { code: 15, name: "oDeliveryNotes", entity_set: "DeliveryNotes", verified: true },
    DocType { code: 16, name: "oReturns", entity_set: "Returns", verified: false },
    DocType { code: 17, name: "oOrders", entity_set: "Orders", verified: false },
    DocType { code: 18, name: "oPurchaseInvoices", entity_set: "PurchaseInvoices", verified: false },
    DocType { code: 19, name: "oPurchaseCreditNotes", entity_set: "PurchaseCreditNotes", verified: false },
    DocType { code: 20, name: "oPurchaseDeliveryNotes", entity_set: "PurchaseDeliveryNotes", verified: false },
    DocType { code: 21, name: "oPurchaseReturns", entity_set: "PurchaseReturns", verified: false },
    DocType { code: 22, name: "oPurchaseOrders", entity_set: "PurchaseOrders", verified: false },
    DocType { code: 23, name: "oQuotations", entity_set: "Quotations", verified: false },
];

pub fn by_code(code: i32) -> Option<&'static DocType> {
    DOC_TYPES.iter().find(|d| d.code == code)
}

pub fn by_set(set: &str) -> Option<&'static DocType> {
    DOC_TYPES.iter().find(|d| d.entity_set == set)
}

/// True when `base_type` refers to a document Coucou can follow back.
pub fn is_followable(base_type: i32) -> bool {
    by_code(base_type).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_notes_are_verified_and_resolve_both_ways() {
        let dt = by_code(15).unwrap();
        assert!(dt.verified);
        assert_eq!(dt.entity_set, "DeliveryNotes");
        assert_eq!(by_set("Invoices").unwrap().code, 13);
    }

    #[test]
    fn unknown_codes_are_not_followed() {
        assert!(!is_followable(999));
        assert!(is_followable(17));
    }
}
