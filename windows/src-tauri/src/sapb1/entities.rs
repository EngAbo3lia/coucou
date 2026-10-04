//! The writable entities beyond the sales/purchase documents, described once so
//! the planner, the confirm dialog and the write command all agree on what a
//! given entity needs.
//!
//! Each entity names its header fields (what to ask the user) and whether it
//! also takes document lines. `build_payload` turns the filled-in values into
//! the Service Layer body; `required_missing` is the guard the dialog relies on
//! before anything is posted.

use serde_json::{json, Map, Value};

use super::documents::Line;

/// A value the user is asked for in the confirm dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    /// `"text"`, `"number"`, `"date"` or `"choice"`.
    pub kind: &'static str,
    pub required: bool,
    /// Prefilled value, so the common case is one click.
    pub default: &'static str,
    /// Allowed values for `"choice"`; empty otherwise.
    pub options: &'static [&'static str],
}

/// An entity the assistant can create.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDef {
    pub set: &'static str,
    pub name: &'static str,
    pub fields: &'static [Field],
    /// Whether document lines are required as well as the header fields.
    pub needs_lines: bool,
}

const CARD_TYPES: &[&str] = &["cCustomer", "cSupplier", "cLead"];

pub const ENTITIES: &[EntityDef] = &[
    EntityDef {
        set: "Items",
        name: "item",
        fields: &[
            Field { key: "ItemCode", label: "Item code", kind: "text", required: true, default: "", options: &[] },
            Field { key: "ItemName", label: "Item name", kind: "text", required: true, default: "", options: &[] },
            Field { key: "ItemsGroupCode", label: "Item group", kind: "number", required: false, default: "100", options: &[] },
        ],
        needs_lines: false,
    },
    EntityDef {
        set: "BusinessPartners",
        name: "business partner",
        fields: &[
            Field { key: "CardCode", label: "Card code", kind: "text", required: true, default: "", options: &[] },
            Field { key: "CardName", label: "Name", kind: "text", required: true, default: "", options: &[] },
            Field { key: "CardType", label: "Type", kind: "choice", required: true, default: "cCustomer", options: CARD_TYPES },
            Field { key: "Country", label: "Country", kind: "text", required: true, default: "US", options: &[] },
            Field { key: "Currency", label: "Currency", kind: "text", required: true, default: "GBP", options: &[] },
            Field { key: "BillToState", label: "Billing state", kind: "text", required: true, default: "CA", options: &[] },
        ],
        needs_lines: false,
    },
    EntityDef {
        set: "InventoryGenEntries",
        name: "goods receipt",
        fields: &[
            Field { key: "DocDate", label: "Date", kind: "date", required: true, default: "", options: &[] },
            Field { key: "WarehouseCode", label: "Warehouse", kind: "text", required: true, default: "01", options: &[] },
        ],
        needs_lines: true,
    },
    EntityDef {
        set: "ExchangeRates",
        name: "exchange rate",
        fields: &[
            Field { key: "RateDate", label: "Date", kind: "date", required: true, default: "", options: &[] },
            Field { key: "Currency", label: "Currency", kind: "text", required: true, default: "", options: &[] },
            Field { key: "Rate", label: "Rate", kind: "decimal", required: true, default: "1", options: &[] },
        ],
        needs_lines: false,
    },
];

pub fn find(set: &str) -> Option<&'static EntityDef> {
    ENTITIES.iter().find(|e| e.set == set)
}

pub fn value_for<'a>(values: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    values.get(key).and_then(Value::as_str)
}

/// The header fields of this entity that the user left empty.
pub fn required_missing(entity: &EntityDef, values: &Map<String, Value>) -> Vec<&'static str> {
    entity
        .fields
        .iter()
        .filter(|f| f.required && value_for(values, f.key).map(|v| v.trim().is_empty()).unwrap_or(true))
        .map(|f| f.label)
        .collect()
}

/// The Service Layer body for an entity and its filled-in values.
pub fn build_payload(entity: &EntityDef, values: &Map<String, Value>, lines: &[Line]) -> Result<Value, String> {
    let mut body = Map::new();
    for field in entity.fields {
        // Only send what the user actually provided; the defaults are already
        // filled in by the dialog, so a blank optional field is left out.
        if let Some(v) = value_for(values, field.key) {
            if v.trim().is_empty() {
                continue;
            }
            let value = if field.kind == "number" {
                json!(v.trim().parse::<i64>().map_err(|_| format!("{} must be a whole number.", field.label))?)
            } else if field.kind == "decimal" {
                json!(v.trim().parse::<f64>().map_err(|_| format!("{} must be a number.", field.label))?)
            } else {
                json!(v.trim())
            };
            body.insert(field.key.to_string(), value);
        }
    }

    match entity.set {
        "Items" => {
            // A group always has to be present; 100 is the usual default.
            if !body.contains_key("ItemsGroupCode") {
                body.insert("ItemsGroupCode".into(), json!(100));
            }
            body.insert("InventoryItem".into(), json!("tYES"));
            body.insert("SalesItem".into(), json!("tYES"));
            body.insert("PurchaseItem".into(), json!("tYES"));
        }
        "InventoryGenEntries" => {
            let warehouse = value_for(values, "WarehouseCode").unwrap_or("01").to_string();
            let doc_lines: Vec<Value> = lines
                .iter()
                .filter(|l| !l.item_code.is_empty())
                .map(|l| {
                    let mut line = json!({
                        "ItemCode": l.item_code,
                        "Quantity": l.quantity,
                        "WarehouseCode": warehouse,
                        "UnitPrice": l.price.unwrap_or(0.0),
                    });
                    if let Some(batch) = l.batch.as_ref().filter(|b| !b.trim().is_empty()) {
                        line["BatchNumbers"] = json!([{ "BatchNumber": batch.trim(), "Quantity": l.quantity }]);
                    }
                    let serials: Vec<Value> = l
                        .serials
                        .iter()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                        .map(|s| json!({ "SerialNumber": s, "Quantity": 1.0 }))
                        .collect();
                    if !serials.is_empty() {
                        line["SerialNumbers"] = json!(serials);
                    }
                    line
                })
                .collect();
            body.insert("DocumentLines".into(), json!(doc_lines));
        }
        _ => {}
    }

    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).into(), json!(v));
        }
        m
    }

    #[test]
    fn every_entity_is_looked_up_by_its_set() {
        for e in ENTITIES {
            assert_eq!(find(e.set).map(|x| x.set), Some(e.set));
        }
        assert!(find("Nope").is_none());
    }

    #[test]
    fn a_missing_required_field_is_named() {
        let missing = required_missing(find("Items").unwrap(), &values(&[("ItemCode", "X")]));
        assert_eq!(missing, vec!["Item name"]);
        let none = required_missing(find("Items").unwrap(), &values(&[("ItemCode", "X"), ("ItemName", "Y")]));
        assert!(none.is_empty());
    }

    #[test]
    fn an_item_body_always_carries_the_three_flags() {
        let body = build_payload(find("Items").unwrap(), &values(&[("ItemCode", "ZZ1"), ("ItemName", "Demo")]), &[]).unwrap();
        assert_eq!(body["ItemCode"], "ZZ1");
        assert_eq!(body["ItemName"], "Demo");
        assert_eq!(body["InventoryItem"], "tYES");
        assert_eq!(body["SalesItem"], "tYES");
        assert_eq!(body["PurchaseItem"], "tYES");
        assert_eq!(body["ItemsGroupCode"], json!(100));
    }

    #[test]
    fn a_partner_body_keeps_its_choice_and_country() {
        let body = build_payload(
            find("BusinessPartners").unwrap(),
            &values(&[("CardCode", "ZZC"), ("CardName", "Demo"), ("CardType", "cCustomer"), ("Country", "US"), ("Currency", "GBP"), ("BillToState", "CA")]),
            &[],
        )
        .unwrap();
        assert_eq!(body["CardType"], "cCustomer");
        assert_eq!(body["Country"], "US");
        assert_eq!(body["BillToState"], "CA");
        assert!(body.get("ShipToState").is_none());
    }

    #[test]
    fn a_goods_receipt_carries_its_lines_and_warehouse() {
        let lines = vec![Line { item_code: "ZZ1".into(), quantity: 250.0, price: Some(5.0), base_line: None, serials: Vec::new(), batch: None }];
        let body = build_payload(
            find("InventoryGenEntries").unwrap(),
            &values(&[("DocDate", "2026-08-01"), ("WarehouseCode", "02")]),
            &lines,
        )
        .unwrap();
        assert_eq!(body["DocDate"], "2026-08-01");
        assert_eq!(body["DocumentLines"][0]["ItemCode"], "ZZ1");
        assert_eq!(body["DocumentLines"][0]["Quantity"], 250.0);
        assert_eq!(body["DocumentLines"][0]["WarehouseCode"], "02");
    }

    #[test]
    fn a_non_numeric_number_field_is_rejected() {
        let err = build_payload(find("Items").unwrap(), &values(&[("ItemCode", "ZZ1"), ("ItemName", "D"), ("ItemsGroupCode", "abc")]), &[]).unwrap_err();
        assert!(err.contains("whole number"));
    }
}

