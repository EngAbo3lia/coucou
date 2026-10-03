//! Runtime `$metadata` parsing.
//!
//! The generated catalogue (`catalogue.rs`) is the offline default, but the
//! customer's server is the authority. A probe parses `/b1s/v{1,2}/$metadata`
//! and answers what entity sets and fields actually exist there, so a report
//! never assumes a field the installation does not have.
//!
//! The metadata is a large but simple generated XML document: no CDATA, no
//! namespaces that matter to us, no mixed content. Scanning for tags and
//! attributes is enough and keeps the crate dependency-free.

/// An entity set and the type behind it. Several sets share one type: every
/// sales and purchase document is a `Document`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitySet {
    pub name: String,
    pub entity_type: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityType {
    pub name: String,
    pub properties: Vec<String>,
    /// Foreign-key relations only. These are the sole links OData can expand or
    /// cross-join; document-to-document links are not among them.
    pub navigation: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub entity_sets: Vec<EntitySet>,
    pub types: Vec<EntityType>,
}

impl Metadata {
    pub fn entity_set(&self, name: &str) -> Option<&EntitySet> {
        self.entity_sets.iter().find(|s| s.name == name)
    }

    pub fn entity_type(&self, name: &str) -> Option<&EntityType> {
        self.types.iter().find(|t| t.name == name)
    }

    /// Does `field` exist on the type behind entity set `set`?
    pub fn has_field(&self, set: &str, field: &str) -> bool {
        self.entity_set(set)
            .and_then(|s| self.entity_type(&s.entity_type))
            .is_some_and(|t| t.properties.iter().any(|p| p == field))
    }
}

/// Parse a `$metadata` document into the sets and types a report can use.
pub fn parse(xml: &str) -> Metadata {
    let entity_sets = open_tags(xml, "EntitySet")
        .iter()
        .filter_map(|tag| {
            let name = attr(tag, "Name")?;
            let raw = attr(tag, "EntityType")?;
            let entity_type = raw.rsplit('.').next().unwrap_or(&raw).to_string();
            Some(EntitySet { name, entity_type })
        })
        .collect();

    let types = element_blocks(xml, "EntityType ")
        .iter()
        .filter_map(|block| {
            let head = &block[..block.find('>')?];
            Some(EntityType {
                name: attr(head, "Name")?,
                properties: names(block, "Property"),
                navigation: names(block, "NavigationProperty"),
            })
        })
        .collect();

    Metadata { entity_sets, types }
}

fn names(block: &str, tag: &str) -> Vec<String> {
    open_tags(block, tag)
        .iter()
        .filter_map(|t| attr(t, "Name"))
        .collect()
}

/// `Name="..."` inside one tag, regardless of where the attribute sits.
fn attr(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Every `<tag ...>` occurrence, sliced from `<` to the closing `>`.
fn open_tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let needle = format!("<{tag}");
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(offset) = xml[at..].find(&needle) {
        let start = at + offset;
        let Some(close) = xml[start..].find('>') else {
            break;
        };
        out.push(&xml[start..start + close + 1]);
        at = start + close + 1;
    }
    out
}

/// Every `<name ...>...</name>` block, inclusive of both tags.
fn element_blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let close = format!("</{}>", name.trim());
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(offset) = xml[at..].find(&open) {
        let start = at + offset;
        let Some(end_offset) = xml[start..].find(&close) else {
            break;
        };
        let end = start + end_offset + close.len();
        out.push(&xml[start..end]);
        at = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<edmx:Edmx><Schema>
<EntityType Name="Document" OpenType="true">
  <Property Name="DocEntry" Type="Edm.Int32"/>
  <Property Name="DocDate" Type="Edm.DateTime"/>
  <NavigationProperty FromRole="Documents" Name="BusinessPartner" Relationship="SAPB1.FK_Documents_BusinessPartners" ToRole="BusinessPartners"/>
</EntityType>
<EntityType Name="Payment"><Property Name="DocNum" Type="Edm.Int32"/></EntityType>
</Schema><EntityContainer>
<EntitySet EntityType="SAPB1.Document" Name="Orders"/>
<EntitySet EntityType="SAPB1.Document" Name="Invoices"/>
<EntitySet EntityType="SAPB1.Payment" Name="IncomingPayments"/>
</EntityContainer></edmx:Edmx>"#;

    #[test]
    fn sets_share_one_type_and_keep_their_names() {
        let md = parse(SAMPLE);
        assert_eq!(md.entity_sets.len(), 3);
        assert_eq!(md.entity_set("Orders").unwrap().entity_type, "Document");
        assert_eq!(md.entity_set("IncomingPayments").unwrap().entity_type, "Payment");
    }

    #[test]
    fn fields_and_navigation_are_found() {
        let md = parse(SAMPLE);
        assert!(md.has_field("Orders", "DocEntry"));
        assert!(md.has_field("Invoices", "DocDate"));
        assert!(!md.has_field("Invoices", "DocumentLines"));
        assert!(!md.has_field("Orders", "NotAField"));
        let doc = md.entity_type("Document").unwrap();
        assert_eq!(doc.navigation, vec!["BusinessPartner".to_string()]);
    }
}
