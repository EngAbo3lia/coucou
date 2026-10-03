//! What the Service Layer will and will not do, and the query strings that come
//! out of it.
//!
//! The rules below were verified against a live FP 3400 server, not read off a
//! spec. They are the contract a query plan is built on: the planner may only
//! emit what `CAPABILITIES` marks as working, and must reach for the
//! `ALTERNATIVES` when it does not.

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
