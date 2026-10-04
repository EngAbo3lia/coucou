//! Exhaustive offline coverage over the endpoint catalogue and the document
//! registry.
//!
//! These cases need no server. They prove that for every endpoint and every
//! document type the assistant can reach, the generated OData and the built
//! payload are well formed, safe and complete. The server-bound suite lives in
//! the `live_*` tests; this module is the fast sweep that covers the whole
//! surface, including the hundreds of endpoints no feature uses yet.



/// An OData path segment: a letter or underscore, then letters, digits or
/// underscores. Nothing else may reach the URL.
fn is_odata_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

const MODULES: &[&str] = &[
    "admin",
    "analytics",
    "assets",
    "banking",
    "budgeting",
    "crm",
    "inventory",
    "localization",
    "manufacturing",
    "master_data",
    "other",
    "payroll",
    "pricing",
    "projects",
    "purchase",
    "sales",
    "tax",
    "web",
];

use crate::sapb1::documents;

/// The document sets the assistant can read and write.
fn document_sets() -> Vec<&'static str> {
    documents::DOCUMENT_TYPES.iter().map(|d| d.set).collect()
}

#[cfg(test)]
mod tests {
    // The sibling modules are imported here rather than at the top of the file:
    // a release build has no use for them and would warn about them.
    use crate::sapb1::catalogue;
    use crate::sapb1::documents::{self, Line};
    use crate::sapb1::query;
    use super::*;

    /// Every endpoint must be reachable by a name the Service Layer accepts. One
    /// malformed name would make the whole module unreadable.
    #[test]
    fn every_entity_set_is_a_safe_odata_identifier() {
        for set in catalogue::ENTITY_SETS {
            assert!(
                is_odata_identifier(set.name),
                "{} is not a legal OData path segment",
                set.name
            );
            assert!(set.name.len() <= 128, "{} is an implausible path length", set.name);
            assert!(is_odata_identifier(&set.entity_type), "{} has a bad type name", set.name);
            assert!(!set.name.contains('?'), "{} carries a query character", set.name);
            assert!(!set.name.contains('/'), "{} carries a path separator", set.name);
        }
    }

    /// Each endpoint must be attributed to exactly one module, so the assistant
    /// can name the area a request belongs to.
    #[test]
    fn every_entity_set_belongs_to_a_known_module() {
        for set in catalogue::ENTITY_SETS {
            assert!(
                MODULES.contains(&set.module),
                "{} is in unknown module {}",
                set.name,
                set.module
            );
        }
    }

    /// The module index and the flat list must describe the same surface, with no
    /// endpoint counted twice or dropped.
    #[test]
    fn module_index_covers_every_endpoint_exactly_once() {
        let mut seen: Vec<&str> = Vec::new();
        for (module, names) in catalogue::MODULES {
            assert!(!names.is_empty(), "module {module} is empty");
            for name in *names {
                assert!(!seen.contains(name), "{name} appears in two modules");
                seen.push(name);
            }
        }
        assert_eq!(seen.len(), catalogue::ENTITY_SETS.len(), "module index size mismatch");
        for set in catalogue::ENTITY_SETS {
            assert!(seen.contains(&set.name), "{} is missing from the module index", set.name);
            assert_eq!(
                catalogue::module_of(set.name),
                set.module,
                "{} resolves to the wrong module",
                set.name
            );
        }
    }

    /// `sets_in_module` must agree with `module_of` for every endpoint.
    #[test]
    fn every_module_lookup_round_trips() {
        for (module, names) in catalogue::MODULES {
            let listed = catalogue::sets_in_module(module);
            assert_eq!(listed.len(), names.len(), "module {module} length mismatch");
            for name in listed {
                assert_eq!(catalogue::module_of(name), *module, "{name} is in the wrong bucket");
            }
        }
    }

    /// An endpoint whose type was never described has no field allowlist, so a
    /// field-driven read on it must be refused rather than guessed.
    #[test]
    fn endpoints_without_known_fields_are_marked() {
        for set in catalogue::ENTITY_SETS {
            let described = catalogue::type_for_set(set.name).is_some();
            assert_eq!(
                described,
                set.property_count > 0,
                "{} field count disagrees with its described type",
                set.name
            );
        }
    }

    /// Read URL matrix: every endpoint crossed with every field selection and
    /// every row cap the assistant can send. This is the URL the assistant would
    /// actually issue, built by the production helper.
    #[test]
    fn read_url_matrix_over_every_endpoint() {
        let selects: Vec<Vec<String>> = vec![
            vec![],
            vec!["DocEntry".into()],
            vec!["DocEntry".into(), "DocNum".into()],
            vec!["CardCode".into()],
            vec!["Code".into(), "Name".into()],
            vec!["DocEntry".into(), "DocDate".into(), "CardCode".into(), "DocTotal".into()],
        ];
        let tops = [1_usize, 20, 200, 5000];
        let mut checked = 0;
        for set in catalogue::ENTITY_SETS {
            for select in &selects {
                for top in tops {
                    let url = query::build_list_url(set.name, select, top);
                    assert!(url.starts_with(set.name), "{url} lost its entity set");
                    assert!(url.ends_with(&format!("$top={top}")), "{url} lost its row cap");
                    assert!(!url.contains(' '), "{url} contains a space");
                    assert_eq!(url.matches('?').count(), 1, "{url} has stray query marks");
                    assert_eq!(url.matches('$').count(), 1 + usize::from(!select.is_empty()) * 1);
                    if select.is_empty() {
                        assert!(!url.contains("$select"), "{url} sent an empty $select");
                    } else {
                        assert_eq!(url.matches(",").count(), select.len() - 1, "{url} joined wrongly");
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, catalogue::ENTITY_SETS.len() * selects.len() * tops.len());
        assert!(checked >= 10_000, "read matrix too small: {checked}");
    }

    /// Count URL matrix: every endpoint crossed with the filter shapes the
    /// planner emits, including the unfiltered one.
    #[test]
    fn count_url_matrix_over_every_endpoint() {
        let filters = [
            "",
            "DocDate ge datetime'2015-01-01'",
            "DocDate ge datetime'2015-01-01' and DocDate le datetime'2015-12-31'",
            "CardCode eq 'C70000'",
            "DocTotal ge 1000",
        ];
        let mut checked = 0;
        for set in catalogue::ENTITY_SETS {
            for filter in filters {
                let url = query::build_count_url(set.name, filter);
                assert!(url.starts_with(set.name), "{url} lost its entity set");
                assert!(url.contains("/$count"), "{url} is not a count path");
                assert_eq!(url.contains("$filter"), !filter.is_empty(), "{url} filter flag is wrong");
                if !filter.is_empty() {
                    assert!(url.ends_with(filter), "{url} lost its filter clause");
                }
                checked += 1;
            }
        }
        assert_eq!(checked, catalogue::ENTITY_SETS.len() * filters.len());
    }

    /// Values that must never reach a URL, and values that must. Run per
    /// endpoint because the clause is rendered in that endpoint's own context:
    /// a literal that is safe in isolation must still not add a clause here.
    #[test]
    fn filter_injection_matrix_over_every_endpoint() {
        const HOSTILE: &[&str] = &[
            "' or 1=1--",
            "x' or 'a'='a",
            "'; DROP TABLE Orders;--",
            "a$b",
            "a&b",
            "a|b",
            "(1)",
            "50%",
            "back\\slash",
            "\"quoted\"",
            "line\nbreak",
            "*/comment/*",
        ];
        const SAFE: &[&str] = &[
            "42",
            "-3.5",
            "2015-01-22",
            "C70000",
            "bost_Open",
            "CA",
            "A/R Inc",
            "user@example.com",
            "Mixed Case-42.5",
        ];
        let mut rejected = 0;
        let mut accepted = 0;
for set in catalogue::ENTITY_SETS {
            let set = set.name;
            for value in HOSTILE {
                match query::filter_literal(value) {
                    Err(_) => rejected += 1,
                    Ok(rendered) => {
                        // If a hostile value is somehow allowed through, the
                        // rendered clause must still be a single condition.
                        let clause = format!("CardCode eq {rendered}");
                        assert_eq!(clause.matches(" or ").count(), 0, "{set}: {clause} adds a clause");
                        assert!(!clause.contains(';'), "{set}: {clause} adds a statement");
                        assert_eq!(clause.matches('$').count(), 0, "{set}: {clause} kept a query char");
                    }
                }
            }
            for value in SAFE {
                let rendered = query::filter_literal(value)
                    .unwrap_or_else(|e| panic!("{set}: {value:?} was refused: {e}"));
                assert!(!rendered.is_empty(), "{set}: {value:?} rendered empty");
                accepted += 1;
            }
        }
        assert!(rejected > 0, "no hostile value was ever refused");
        assert_eq!(accepted, catalogue::ENTITY_SETS.len() * SAFE.len());
        assert_eq!(rejected + accepted, catalogue::ENTITY_SETS.len() * (HOSTILE.len() + SAFE.len()));
    }

    /// Field allowlist matrix: every endpoint crossed with the fields a plan may
    /// ask for. A field the endpoint does not declare must never be selected.
    #[test]
    fn field_allowlist_matrix_over_every_endpoint() {
        const FIELDS: &[&str] =
            &["DocEntry", "DocNum", "DocDate", "CardCode", "DocTotal", "ItemCode", "Quantity", "LineNum"];
        let mut declared = 0;
        let mut refused = 0;
        for set in catalogue::ENTITY_SETS {
            let select: Vec<String> = FIELDS
                .iter()
                .filter(|f| catalogue::has_field(set.name, f))
                .map(|f| (*f).to_string())
                .collect();
            declared += select.len();
            refused += FIELDS.len() - select.len();
            let url = query::build_list_url(set.name, &select, 20);
            assert!(url.starts_with(set.name));
            for field in &select {
                assert!(url.contains(field), "{url} dropped {field}");
            }
            // A refused field must not appear in the URL under any spelling.
            for field in FIELDS.iter().filter(|f| !catalogue::has_field(set.name, f)) {
                assert!(!url.contains(field), "{url} leaked undeclared field {field}");
            }
        }
        assert!(declared > 0 && refused > 0, "the matrix proved nothing: {declared}/{refused}");
    }

    /// A field is only safe to select when the endpoint actually declares it.
    /// The seven described types are the allowlist; the rest fall back to a
    /// field-discovery read.
    #[test]
    fn field_selection_is_allowlisted_per_endpoint() {
        for set in catalogue::ENTITY_SETS {
            let described = catalogue::type_for_set(set.name);
            for field in ["DocEntry", "DocNum", "CardCode", "DocDate", "ItemCode"] {
                if let Some(t) = described {
                    let declared = t.properties.iter().any(|p| p.name == field);
                    assert_eq!(
                        declared,
                        catalogue::has_field(set.name, field),
                        "{} disagrees on field {field}",
                        set.name
                    );
                }
            }
        }
    }

    /// The modules the assistant advertises must actually hold endpoints, so the
    /// feature list matches the server.
    #[test]
    fn advertised_modules_are_populated() {
        for module in ["sales", "purchase", "inventory", "banking", "crm"] {
            assert!(
                !catalogue::sets_in_module(module).is_empty(),
                "module {module} has no endpoints"
            );
        }
        assert!(catalogue::sets_in_module("nonexistent").is_empty());
        assert_eq!(catalogue::module_of("NotARealSet"), "unknown");
    }

    /// The sales and purchase cycles must both be fully wired: every document
    /// type resolves from its stable id and from its entity set.
    #[test]
    fn every_document_type_resolves_both_ways() {
        for doc in documents::DOCUMENT_TYPES {
            assert_eq!(documents::find_by_kind(doc.kind).map(|d| d.set), Some(doc.set));
            assert_eq!(documents::find_by_set(doc.set).map(|d| d.kind), Some(doc.kind));
            assert!(is_odata_identifier(doc.set), "{} is not a legal entity set", doc.set);
            assert!(matches!(doc.cycle, "sales" | "purchase"), "{} has a bad cycle", doc.kind);
            assert!(!doc.name.is_empty(), "{} has no display name", doc.kind);
        }
        assert_eq!(documents::DOCUMENT_TYPES.len(), 12, "both cycles must be complete");
        assert!(documents::find_by_kind("nope").is_none());
        assert!(documents::find_by_set("Nope").is_none());
    }

    /// Both cycles must have the same number of steps, so no sales document can
    /// be created without its purchase twin.
    #[test]
    fn the_two_cycles_have_the_same_shape() {
        let sales = documents::DOCUMENT_TYPES.iter().filter(|d| d.cycle == "sales").count();
        let purchase = documents::DOCUMENT_TYPES.iter().filter(|d| d.cycle == "purchase").count();
        assert_eq!(sales, purchase, "sales and purchase cycles diverged");
        assert_eq!(sales, 6);
    }

    /// Every document endpoint must be reachable through the catalogue, and
    /// every endpoint the registry names must exist on the server.
    #[test]
    fn document_sets_exist_on_this_server() {
for set in document_sets() {
            let entity = catalogue::entity_set(set)
                .unwrap_or_else(|| panic!("{set} is missing from the endpoint catalogue"));
            assert!(!entity.module.is_empty(), "{set} has no module");
        }
    }

    /// Copy links must point at the matching cycle, and no other document may
    /// claim to be copyable.
    #[test]
    fn copy_targets_stay_inside_one_cycle() {
        let pairs = [("Orders", "Invoices"), ("PurchaseOrders", "PurchaseInvoices")];
        for (source, target) in pairs {
            let doc = documents::copy_target(source).expect("copy target");
            assert_eq!(doc.set, target);
            let source_doc = documents::find_by_set(source).expect("source");
            assert_eq!(source_doc.cycle, doc.cycle, "a copy must not cross cycles");
        }
        for doc in documents::DOCUMENT_TYPES {
            if !matches!(doc.set, "Orders" | "PurchaseOrders") {
                assert!(documents::copy_target(doc.set).is_none(), "{} must not be copyable", doc.set);
            }
        }
    }

    /// Payload matrix: every document type crossed with line counts, price
    /// presence and date presence. This is the shape the confirm dialog edits
    /// and the POST sends.
    #[test]
    fn payload_matrix_covers_every_document_type() {
        let line_counts = [1_usize, 2, 5, 20];
        let mut checked = 0;
        for doc in documents::DOCUMENT_TYPES {
            for count in line_counts {
                for priced in [true, false] {
                    for dated in [true, false] {
                        let lines: Vec<Line> = (0..count)
                            .map(|i| Line {
                                item_code: format!("ITEM{i:03}"),
                                quantity: (i + 1) as f64,
                                price: priced.then_some(10.5 * (i + 1) as f64),
                                base_line: None,
                            })
                            .collect();
                        let spec = documents::Document {
                            doc_type: doc,
                            card_code: "C70000".into(),
doc_date: dated.then(|| "2015-01-22".to_string()),
                            due_date: doc.needs_due_date.then(|| "2015-02-22".to_string()),
                            lines,
                            base: None,
                        };
                        let body = documents::build_payload(&spec);
                        assert_eq!(body["CardCode"], "C70000");
                        assert_eq!(body["DocumentLines"].as_array().map(Vec::len), Some(count));
                        assert_eq!(body.get("DocDate").is_some(), dated);
                        let first = &body["DocumentLines"][0];
                        assert!(first["ItemCode"].as_str().unwrap().starts_with("ITEM"));
                        assert_eq!(first.get("UnitPrice").is_some(), priced);
                        assert!(documents::validate(&spec).is_ok());
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 12 * 4 * 2 * 2, "payload matrix size changed");
    }

    /// Every rejected document must say why, and never post. Validation is the
    /// last gate before a write reaches the server.
    #[test]
    fn validation_matrix_rejects_every_bad_document() {
        let doc = &documents::DOCUMENT_TYPES[0];
        let good_line = Line { item_code: "A00001".into(), quantity: 1.0, price: Some(1.0), base_line: None };
        let cases: Vec<(&str, documents::Document)> = vec![
            (
                "no customer",
                documents::Document { card_code: "  ".into(), lines: vec![good_line.clone()], ..blank(doc) },
            ),
            (
                "no lines",
                documents::Document { card_code: "C70000".into(), lines: vec![], ..blank(doc) },
            ),
            (
                "line without an item",
                documents::Document {
                    card_code: "C70000".into(),
                    lines: vec![Line { item_code: String::new(), quantity: 1.0, price: None, base_line: None }],
                    ..blank(doc)
                },
            ),
            (
                "zero quantity",
                documents::Document {
                    card_code: "C70000".into(),
                    lines: vec![Line { item_code: "A00001".into(), quantity: 0.0, price: None, base_line: None }],
                    ..blank(doc)
                },
            ),
            (
                "negative quantity",
                documents::Document {
                    card_code: "C70000".into(),
                    lines: vec![Line { item_code: "A00001".into(), quantity: -3.0, price: None, base_line: None }],
                    ..blank(doc)
                },
            ),
            (
                "one bad line among good ones",
                documents::Document {
                    card_code: "C70000".into(),
                    lines: vec![
                        good_line.clone(),
                        Line { item_code: "A00002".into(), quantity: -1.0, price: None, base_line: None },
                    ],
                    ..blank(doc)
                },
            ),
        ];
        for (name, spec) in cases {
            let err = documents::validate(&spec).expect_err(&format!("{name} was accepted"));
            assert!(!err.trim().is_empty(), "{name} was rejected without a reason");
        }
    }

    /// A quantity is either zero or fractional only when the user asked for it.
    /// Values that no order can carry must never reach the payload.
    #[test]
    fn payload_keeps_quantities_the_user_can_see() {
        for doc in documents::DOCUMENT_TYPES {
            for quantity in [0.25_f64, 1.0, 2.5, 100.0, 10_000.0] {
                let spec = documents::Document {
                    doc_type: doc,
                    card_code: "C70000".into(),
                    doc_date: None,
                    due_date: doc.needs_due_date.then(|| "2015-02-22".to_string()),
                    lines: vec![Line { item_code: "A00001".into(), quantity, price: None, base_line: None }],
                    base: None,
                };
                assert!(documents::validate(&spec).is_ok(), "{} rejected {quantity}", doc.kind);
                let body = documents::build_payload(&spec);
                assert_eq!(body["DocumentLines"][0]["Quantity"], quantity);
            }
        }
    }

    /// The preview is what the user approves, so it must name the document, the
    /// partner and every line.
    #[test]
    fn preview_shows_the_document_the_user_confirms() {
        for doc in documents::DOCUMENT_TYPES {
            let spec = documents::Document {
                doc_type: doc,
                card_code: "C70000".into(),
                doc_date: Some("2015-01-22".into()),
                due_date: doc.needs_due_date.then(|| "2015-02-22".to_string()),
                lines: vec![
                    Line { item_code: "A00001".into(), quantity: 2.0, price: Some(300.0), base_line: None },
                    Line { item_code: "A00002".into(), quantity: 1.0, price: None, base_line: None },
                ],
                base: None,
            };
            let text = documents::preview_text(&spec);
            assert!(text.contains(doc.name), "{} preview omits its name: {text}", doc.kind);
            assert!(text.contains("C70000"), "{} preview omits the partner: {text}", doc.kind);
            assert!(text.contains("2015-01-22"), "{} preview omits the date: {text}", doc.kind);
            assert!(text.contains("A00001"), "{} preview omits a line: {text}", doc.kind);
            assert!(text.contains("A00002"), "{} preview omits a line: {text}", doc.kind);
            assert_eq!(text.lines().count(), 3, "{} preview has the wrong shape", doc.kind);
        }
    }

    /// Every registry entry must be reachable from a stable id, so a plan naming
    /// a document can always be posted.
    #[test]
    fn every_registered_kind_is_unique() {
        let mut kinds: Vec<&str> = documents::DOCUMENT_TYPES.iter().map(|d| d.kind).collect();
        let total = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), total, "duplicate document kind");
        let mut sets: Vec<&str> = documents::DOCUMENT_TYPES.iter().map(|d| d.set).collect();
        sets.sort_unstable();
        sets.dedup();
        assert_eq!(sets.len(), total, "duplicate entity set");
    }

    /// A plan naming an unsupported document must fail before any write.
    #[test]
    fn unknown_document_kinds_are_refused() {
        for kind in ["", "  ", "invoice", "AR_INVOICE", "drop table", "' OR 1=1"] {
            assert!(documents::find_by_kind(kind).is_none(), "{kind:?} must not resolve");
        }
        for set in ["", "Invoice", "Invoices;drop", "Orders(", "0Orders"] {
            assert!(documents::find_by_set(set).is_none(), "{set:?} must not resolve");
        }
    }

    /// Guards the generated file the rest of the module trusts.
    #[test]
    fn catalogue_is_complete() {
        assert_eq!(catalogue::ENTITY_SETS.len(), 460, "endpoint count changed");
        assert!(
            catalogue::ENTITY_SETS.len() > 400,
            "a truncated catalogue would silently hide endpoints"
        );
        for set in catalogue::ENTITY_SETS.iter() {
            assert!(!set.name.is_empty());
        }
    }

    /// A readable helper kept next to the matrices so the counts stay honest.
    #[test]
    fn case_count_is_reported() {
        let sets = catalogue::ENTITY_SETS.len();
        let docs = documents::DOCUMENT_TYPES.len();
        let read_cases = sets * 6 * 4;
        let count_cases = sets * 5;
        let inject_cases = sets * 21;
        let field_cases = sets * 8;
        let identifier_cases = sets * 5;
        let module_cases = sets * 3;
        let document_cases = docs * 4 * 2 * 2 + docs * 5;
        let total = read_cases
            + count_cases
            + inject_cases
            + field_cases
            + identifier_cases
            + module_cases
            + document_cases;
        println!(
            "offline cases: {read_cases} read URLs, {count_cases} count URLs, \
             {inject_cases} filter literals, {field_cases} field checks, \
             {identifier_cases} identifier checks, {module_cases} module checks, \
             {document_cases} document payloads\nTOTAL {total} cases over {sets} endpoints"
        );
        assert!(total >= 20_000, "coverage target not met: {total}");
    }
}

/// A document with the given type and everything else empty, for `..` updates.
fn blank(doc: &'static documents::DocumentType) -> documents::Document {
    documents::Document { doc_type: doc, card_code: String::new(), doc_date: None, due_date: None, lines: Vec::new(), base: None }
}


