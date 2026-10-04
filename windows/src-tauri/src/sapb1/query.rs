//! What the Service Layer will and will not do, and the query strings that come
//! out of it.
//!
//! The rules below were verified against a live FP 3400 server, not read off a
//! spec. They are the contract a query plan is built on: the planner may only
//! emit what `CAPABILITIES` marks as working, and must reach for the
//! `ALTERNATIVES` when it does not.

/// The most rows a single read may pull. A lower-bound guard, not a filter: the
/// caller still pages or filters when it needs more.
pub const ROW_CAP: usize = 5000;

/// How many rows to ask for in one request. The server caps a page well below
/// this (20 on the verified install) and returns `odata.nextLink`; the caller
/// pages with `$skip`, so this is a request, never a guarantee.
pub const PAGE: usize = 500;

/// Builds the read URL for an entity set.
///
/// `select` may be empty, in which case the row cap alone is sent: some servers
/// reject a `$select` naming no field. Field names are validated against the
/// endpoint catalogue by the caller, so this only has to join them safely.
pub fn build_list_url(set: &str, select: &[String], top: usize) -> String {
    if select.is_empty() {
        return format!("{set}?$top={top}");
    }
    format!("{set}?$select={}&$top={top}", select.join(","))
}

/// The read URL for one page. `$skip` applies before `$top`, and the server
/// caps the page regardless of the `$top` asked for, so paging is mandatory for
/// anything larger than one page.
pub fn build_page_url(set: &str, select: &[String], top: usize, skip: usize) -> String {
    let base = if select.is_empty() {
        format!("{set}?$top={top}")
    } else {
        format!("{set}?$select={}&$top={top}", select.join(","))
    };
    if skip == 0 {
        base
    } else {
        format!("{base}&$skip={skip}")
    }
}

/// Builds the `$count` URL, optionally with an already-rendered filter clause.
/// The count endpoint is a different path from the list, and takes the filter
/// as one `?$filter=` argument rather than as part of the collection path.
pub fn build_count_url(set: &str, filter: &str) -> String {
    if filter.is_empty() {
        format!("{set}/$count")
    } else {
        format!("{set}/$count?$filter={filter}")
    }
}

/// Renders one OData literal, refusing anything that could terminate it and
/// change the meaning of the query.
///
/// Every value the planner or the user supplies reaches the URL through here.
/// The test is an allowlist, not a denylist: a value is a number, an ISO date,
/// or a string built only from characters that cannot end a literal or open a
/// new clause. Injection is impossible by construction, not by escaping.
pub fn filter_literal(value: &str) -> Result<String, String> {
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

/// Whether a value is a bare ISO date, `YYYY-MM-DD`, which OData wants as a
/// `datetime'...'` literal rather than a string.
fn is_iso_date(v: &str) -> bool {
    v.len() == 10
        && v.as_bytes()[4] == b'-'
        && v.as_bytes()[7] == b'-'
        && v[..4].bytes().all(|b| b.is_ascii_digit())
        && v[5..7].bytes().all(|b| b.is_ascii_digit())
        && v[8..].bytes().all(|b| b.is_ascii_digit())
}

/// Whether the server accepts a query option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Works,
    Fails,
}

#[derive(Debug, Clone, Copy)]
pub struct Capability {
    pub option: &'static str,
    pub example: &'static str,
    pub support: Support,
    pub note: &'static str,
}

/// Verified query-option matrix. Both v1 (OData v3) and v2 (OData v4) were
/// tried; the annotation says where they differ.
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        option: "$select",
        example: "Orders?$select=DocNum,DocDate,CardCode,DocTotal&$top=10",
        support: Support::Works,
        note: "Suppresses the document line collections; omit it to receive them.",
    },
    Capability {
        option: "$filter",
        example: "Orders?$filter=DocDate ge datetime'2024-01-01'&$top=10",
        support: Support::Works,
        note: "Date literals accept a bare date or the datetime'...' form.",
    },
    Capability {
        option: "$top",
        example: "Orders?$top=20",
        support: Support::Works,
        note: "Hard cap per request; page with $skip.",
    },
    Capability {
        option: "$skip",
        example: "Orders?$skip=40&$top=20",
        support: Support::Works,
        note: "$skip applies before $top.",
    },
    Capability {
        option: "$orderby",
        example: "Orders?$orderby=DocTotal desc",
        support: Support::Works,
        note: "Raw fields only; an aggregate alias is rejected.",
    },
    Capability {
        option: "$count",
        example: "Invoices/$count?$filter=DocDate ge 2024-01-01",
        support: Support::Works,
        note: "Exact count, and it does accept $filter — unlike $apply.",
    },
    Capability {
        option: "$expand (single-entity navigation)",
        example: "Orders?$select=DocNum&$expand=BusinessPartner($select=CardCode,CardName)",
        support: Support::Works,
        note: "Only foreign-key relations, e.g. BusinessPartner, Currency.",
    },
    Capability {
        option: "$expand (property collection)",
        example: "Orders?$expand=DocumentLines",
        support: Support::Fails,
        note: "HTTP 400 invalid navigation property: DocumentLines and PaymentInvoices are properties, not navigations.",
    },
    Capability {
        option: "$apply aggregate",
        example: "Invoices?$apply=aggregate(DocTotal with sum as Total,$count as Cnt)",
        support: Support::Works,
        note: "Server-side totals. sum, average and $count all work; v1 and v2 both accept it.",
    },
    Capability {
        option: "$apply groupby",
        example: "Invoices?$apply=groupby((CardCode),aggregate(DocTotal with sum as Total))",
        support: Support::Works,
        note: "One or more raw fields, including a date field; bucket by month in Rust.",
    },
    Capability {
        option: "$apply with $filter",
        example: "Invoices?$filter=DocDate ge 2024-01-01&$apply=aggregate(DocTotal with sum as Total)",
        support: Support::Fails,
        note: "HTTP 400 Not supported query option. This is the central limitation.",
    },
    Capability {
        option: "groupby date functions",
        example: "Invoices?$apply=groupby((year(DocDate),month(DocDate)),aggregate(DocTotal with sum as Total))",
        support: Support::Fails,
        note: "invalid property 'year'; group by the raw DocDate field instead.",
    },
    Capability {
        option: "$orderby aggregate alias",
        example: "Invoices?$apply=groupby((CardCode),aggregate(DocTotal with sum as Total))&$orderby=Total desc",
        support: Support::Fails,
        note: "invalid property 'Total'; sort the aggregate rows in Rust.",
    },
    Capability {
        option: "$crossjoin / QueryService_PostQuery",
        example: "POST QueryService_PostQuery {\"QueryPath\":\"$crossjoin(Orders,Orders/DocumentLines)\",...}",
        support: Support::Fails,
        note: "HTTP 500. Cross-join only works across navigation-property relations, and the document links are not navigations here.",
    },
    Capability {
        option: "plain GET without $select",
        example: "Orders?$top=1",
        support: Support::Works,
        note: "Returns the line collections too (~43 KB for a 5-line order). Heavier, but the only way to reach the lines.",
    },
];

/// When a capability fails, what to do instead.
#[derive(Debug, Clone, Copy)]
pub struct Alternative {
    pub wanted: &'static str,
    pub approach: &'static str,
}

pub const ALTERNATIVES: &[Alternative] = &[
    Alternative {
        wanted: "totals for a date range",
        approach: "Group by the raw day field and slice the range in Rust: groupby((DocDate),aggregate(...)) returns one row per day, which is small.",
    },
    Alternative {
        wanted: "top customers or vendors",
        approach: "Group by CardCode, then sort the rows in Rust; the server rejects orderby on an aggregate alias.",
    },
    Alternative {
        wanted: "monthly trend",
        approach: "Group by DocDate and bucket into months in Rust; year()/month() are unavailable in groupby.",
    },
    Alternative {
        wanted: "document line detail",
        approach: "GET the document without $select to receive DocumentLines, page with $top/$skip, and read BaseType/BaseEntry to follow the chain.",
    },
    Alternative {
        wanted: "payment applied to an invoice",
        approach: "GET IncomingPayments without $select and read PaymentInvoices[].DocEntry and SumApplied.",
    },
    Alternative {
        wanted: "receivables without joining documents",
        approach: "BusinessPartners carries CurrentAccountBalance, OpenOrdersBalance, OpenDeliveryNotesBalance and OpenChecksBalance directly.",
    },
];

/// A field a report groups by or sums.
#[derive(Debug, Clone, Copy)]
pub struct Metric {
    pub field: &'static str,
    pub op: MetricOp,
    pub alias: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricOp {
    Sum,
    Average,
    Count,
}

impl MetricOp {
    fn as_str(self) -> &'static str {
        match self {
            MetricOp::Sum => "sum",
            MetricOp::Average => "average",
            MetricOp::Count => "$count",
        }
    }
}

/// `$apply=groupby((dims),aggregate(metrics))` for the fields given.
pub fn group_by(dimensions: &[&str], metrics: &[Metric]) -> String {
    let dims = dimensions.join(",");
    format!("$apply=groupby(({dims}),aggregate({}))", aggregates(metrics))
}

/// `$apply=aggregate(metrics)` for a whole set, no grouping.
pub fn aggregate(metrics: &[Metric]) -> String {
    format!("$apply=aggregate({})", aggregates(metrics))
}

fn aggregates(metrics: &[Metric]) -> String {
    metrics
        .iter()
        .map(|m| match m.op {
            MetricOp::Count => format!("$count as {}", m.alias),
            _ => format!("{} with {} as {}", m.field, m.op.as_str(), m.alias),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The fields each report entity contributes. Anything outside these sets is
/// rejected before a request is built.
pub const REPORT_FIELDS: &[(&str, &[&str])] = &[
    ("Orders", &["DocEntry", "DocNum", "DocDate", "DocDueDate", "CardCode", "CardName", "DocTotal", "DocCurrency", "DocumentStatus"]),
    ("Invoices", &["DocEntry", "DocNum", "DocDate", "DocDueDate", "CardCode", "CardName", "DocTotal", "DocCurrency", "DocumentStatus"]),
    ("CreditNotes", &["DocEntry", "DocNum", "DocDate", "CardCode", "CardName", "DocTotal", "DocCurrency"]),
    ("DeliveryNotes", &["DocEntry", "DocNum", "DocDate", "CardCode", "CardName", "DocTotal"]),
    ("IncomingPayments", &["DocEntry", "DocNum", "DocDate", "CardCode", "CardName", "TransferSum", "CashSum", "DocCurrency"]),
    ("PurchaseOrders", &["DocEntry", "DocNum", "DocDate", "DocDueDate", "CardCode", "CardName", "DocTotal", "DocCurrency", "DocumentStatus"]),
    ("PurchaseInvoices", &["DocEntry", "DocNum", "DocDate", "DocDueDate", "CardCode", "CardName", "DocTotal", "DocCurrency"]),
    ("VendorPayments", &["DocEntry", "DocNum", "DocDate", "CardCode", "CardName", "TransferSum", "CashSum", "DocCurrency"]),
    ("BusinessPartners", &["CardCode", "CardName", "CardType", "CurrentAccountBalance", "OpenOrdersBalance", "OpenDeliveryNotesBalance", "OpenChecksBalance", "CreditLimit"]),
    ("Items", &["ItemCode", "ItemName", "ItemsGroupCode", "QuantityOnStock", "AvgStdPrice", "MovingAveragePrice", "SalesUnit", "PurchaseUnit"]),
    ("ItemGroups", &["Number", "GroupName"]),
    // Count-only sets: `$count` works on them, but no report field is validated.
    ("EmployeesInfo", &[]),
];

pub fn report_fields(set: &str) -> Option<&'static [&'static str]> {
    REPORT_FIELDS.iter().find(|(name, _)| *name == set).map(|(_, fields)| *fields)
}

/// Is `field` something a report may read from `set`?
pub fn is_report_field(set: &str, field: &str) -> bool {
    report_fields(set).is_some_and(|fields| fields.contains(&field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_url_carries_its_skip_only_when_asked() {
        let select = vec!["DocNum".to_string(), "DocDate".to_string()];
        assert_eq!(build_page_url("Invoices", &select, 500, 0), "Invoices?$select=DocNum,DocDate&$top=500");
        assert_eq!(build_page_url("Invoices", &select, 500, 20), "Invoices?$select=DocNum,DocDate&$top=500&$skip=20");
        assert_eq!(build_page_url("Invoices", &[], 500, 40), "Invoices?$top=500&$skip=40");
    }

    #[test]
    fn group_by_builds_the_verified_shape() {
        let q = group_by(
            &["CardCode"],
            &[Metric { field: "DocTotal", op: MetricOp::Sum, alias: "Total" }],
        );
        assert_eq!(q, "$apply=groupby((CardCode),aggregate(DocTotal with sum as Total))");
    }

    #[test]
    fn count_metrics_ignore_their_field() {
        let q = aggregate(&[Metric { field: "", op: MetricOp::Count, alias: "Cnt" }]);
        assert_eq!(q, "$apply=aggregate($count as Cnt)");
    }

    #[test]
    fn report_fields_are_an_allowlist() {
        assert!(is_report_field("Orders", "DocTotal"));
        assert!(!is_report_field("Orders", "DocumentLines"));
        assert!(!is_report_field("Unknown", "DocTotal"));
    }
}
