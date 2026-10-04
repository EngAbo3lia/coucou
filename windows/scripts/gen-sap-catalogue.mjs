// Generates the SAP Business One catalogue as Rust source from a `$metadata`
// dump, so the entity sets, entity types, their fields and their relations live
// in the codebase instead of in a document.
//
//   node scripts/gen-sap-catalogue.mjs --in metadata-v1.xml --out src-tauri/src/sapb1/catalogue.rs
//
// The customer's metadata is fetched from their own server and is never
// committed; only this script and the catalogue it generates are. Credentials
// stay out of both — the caller logs in and hands over the XML file.

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) {
  args.set(process.argv[i].replace(/^--/, ""), process.argv[i + 1]);
}
const input = args.get("in");
const output = args.get("out");
if (!input || !output) {
  console.error("usage: gen-sap-catalogue.mjs --in <metadata.xml> --out <catalogue.rs>");
  process.exit(1);
}

/**
 * Entity sets a report actually calls. Set-driven on purpose: Business One
 * reuses one type for several documents (`Orders`, `Invoices`, `CreditNotes`
 * and the purchase documents are all `SAPB1.Document`), so the entity set is
 * what selects the document kind, not the type.
 */
const CORE_SETS = [
  "Orders",
  "Quotations",
  "DeliveryNotes",
  "Invoices",
  "CreditNotes",
  "Returns",
  "IncomingPayments",
  "PurchaseOrders",
  "PurchaseInvoices",
  "PurchaseCreditNotes",
  "PurchaseDeliveryNotes",
  "PurchaseReturns",
  "VendorPayments",
  "BusinessPartners",
  "Items",
  "ItemGroups",
  "PriceLists",
  "ChartOfAccounts",
];

const xml = readFileSync(resolve(input), "utf8");

const sets = new Map();
for (const m of xml.matchAll(/<EntitySet EntityType="([^"]+)" Name="([^"]+)"/g)) {
  sets.set(m[2], m[1].split(".").pop());
}

/**
 * Which Business One module an endpoint belongs to. Ordered: the first pattern
 * that matches wins, so specific names must precede general ones.
 */
const MODULES = [
  [
    "localization",
    /^(Brazil|India|Korea|SAFT|NotaFiscal|Elster|Datev|Cust|EWayBill|Returno|NCM|DNF|Sefaz|PES|Gst|WTax|Witholding|Withholding|EUDigitalObject|VatReporting)/,
  ],
  ["assets", /^(Asset|Depreciation|FixedAsset|Capitalization)/],
  ["budgeting", /^(Budget|CostCenters?|ProfitCenters?|DistributionRule|ManualDistribution|RecurringPostings)/],
  ["payroll", /^(EmployeesInfo|Employee|Employment|Payroll|Genders|EducationTypes|EmployeeID)/],
  ["projects", /^(Projects|Project)/],
  ["web", /^(WebClient|BrowserWidgets|Cockpit|MenuAbbreviations|ShortLink|PickLists|FormattedSearches|FormPreferences|KnowledgeBaseSolutions|HelpLinks|PersonalData|MultiLanguageTranslations|ExtendedTranslations|DynamicSystemStrings|PredefinedTexts|Remark|Rumori|Select)/],
  [
    "tax",
    /^(Tax|Vat|VAT|Duty|DeductibleTax|DeductionTax|IndExcise|EBooks|CertificateSeries|Withholding|WTax|TaxOffices|Excise)/,
  ],
  [
    "banking",
    /^(Banks|BankAccount|HouseBank|IncomingPayment|OutgoingPayment|VendorPayment|Payment|Payments|Deposit|Dunning|Interest|CashDiscount|Checks|Check|ChecksForPayment|CashFlow|BillOfExchange|BOE|BoE|InternalBank|InternalReconciliations|CentralBankIndicator|Factoring|DoubtfulDebts|FinancialKPI|FiscalPeriods|ClosingDateProcedure|Wire|SEPAS|PaymentRun)/,
  ],
  ["crm", /^(Activities|Activity|Contacts|ContactGroups|Contact|Campaign|Opportunit|Tasks|Interaction|Lead|TargetGroups|DistributionList|DistributionLists|Teams|Team|RecipientStatus|AgentNames|AlertManagements|Reminder)/],
  [
    "manufacturing",
    /^(BillOfMaterials|Production|Resource|Capacities|Capacity|MRP|Recipe|WorkOrder|ShopFloor|Machine|Shop)/,
  ],
  [
    "inventory",
    /^(Items?|ItemGroups|ItemSupplier|ItemCustomer|Inventory|Warehouses|Warehouse|Stock|BinLocation|Batch|Batches|BatchNumber|SerialNumber|BarCode|BarCodes|BoxSets?|Countings|InventoryCountings|CycleCount|Boosters?|AlternateCatNum|LandedCost|UnitOfMeasure|LengthMeasures|WeightMeasures|MaterialGroups?|Manufacturers|MinMax|Catalogs|Attribute|Product)/,
  ],
  [
    "purchase",
    /^(Purchase|Vendors?|Suppliers?|ReqPurchase|PurchaseRequests|EWayBillDocumentTypes)/,
  ],
  [
    "sales",
    /^(Orders|Quotations|DeliveryNotes|Invoices|CreditNotes|Returns|BusinessPartners?|Partners?|Customers?|BlanketAgreements|AdditionalExpenses|Commission|GoodsShipments|GoodsReturnRequest|ReturnRequest|ReturnActions|ReturnReasons|ServiceContracts|ServiceGroups|Sales|Data[A-Z]|Correction|PaymentTerms)/,
  ],
  ["pricing", /^(PriceLists|SpecialPrices|EnhancedDiscountGroups|Currencies|Currency)/],
  ["analytics", /^(KPI|Chart|Report|FinancialReport|WebClientDashboards|Dashboard|Snapshot)/],
  [
    "master_data",
    /^(Genders|Industries|EducationTypes|Territories|Countries|Counties|States|StateGroups|TaxGroups|VatGroups|Sections|Vehicles|Currencies|Marital|Qualification)/,
  ],
  [
    "admin",
    /^(Users|Roles|User|Settings|Company|COINFO|Approval|Authorization|Sessions|Session|Password|Branches|Fiscal|Sequences|Numbering|UDF|U_DVX|UDO_|ValueMapping|CommunicationMeans|PackagesTypes|IntegrationPackages|SQLQueries|SQLViews|Interfaces|Logs?|Queues|ServiceCalls?|Special|Pick|FormattedSearches|Certificate|Insurances|Templates|Statements|Customs|FiscalRegistry|Form1099|1099|NatureOfAssessees|OccurrenceCodes|TerminationReason|Recognition|PaymentBlocks|Electronic|Country|Currencies)/,
  ],
];

function classify(name) {
  for (const [module, pattern] of MODULES) if (pattern.test(name)) return module;
  return "other";
}

const entitySets = [];
for (const [name, entityType] of sets) entitySets.push({ name, entityType, module: classify(name) });
entitySets.sort((a, b) => a.name.localeCompare(b.name));

// Parse every entity type once so each endpoint can report its real field count.
const allTypes = new Map();
for (const m of xml.matchAll(/<EntityType Name="([^"]+)"[^>]*>([\s\S]*?)<\/EntityType>/g)) {
  const properties = [];
  for (const p of m[2].matchAll(/<Property[^>]*\/?>/g)) {
    const tag = p[0];
    const name = tag.match(/Name="([^"]+)"/)?.[1];
    const ty = tag.match(/Type="([^"]+)"/)?.[1];
    if (name && ty) properties.push({ name, ty });
  }
  const navigationTags = [...m[2].matchAll(/<NavigationProperty[^>]*\/?>/g)].map((n) => n[0]);
  allTypes.set(m[1], { properties, navigationTags });
}
for (const s of entitySets) {
  const t = allTypes.get(s.entityType);
  s.propertyCount = t ? t.properties.length : 0;
  s.navigationCount = t ? t.navigationTags.length : 0;
}

function typeBlock(type) {
  const re = new RegExp(`<EntityType Name="${type}"[^>]*>([\\s\\S]*?)</EntityType>`);
  return xml.match(re)?.[1] ?? null;
}

// Every entity type is emitted, not just the core document ones: the assistant
// can be asked about any of the 460 endpoints, and a field allowlist is what
// makes a read or a write safe. Types are keyed by name and carry the entity
// sets that share them, because Business One reuses one type for many documents.
const setsByType = new Map();
for (const s of entitySets) {
  if (!setsByType.has(s.entityType)) setsByType.set(s.entityType, []);
  setsByType.get(s.entityType).push(s.name);
}

const types = {};
const missing = [];
for (const set of CORE_SETS) {
  if (!sets.has(set)) missing.push(set);
}
for (const [type, t] of allTypes) {
  // Attributes are not in a fixed order: a key property is written
  // `<Property Name="DocEntry" Nullable="false" Type="Edm.Int32"/>`, so Name and
  // Type must be searched for individually rather than matched in sequence.
  const properties = [];
  for (const tag of t.properties) {
    properties.push({ name: tag.name, ty: tag.ty });
  }
  const navigation = [];
  for (const tag of t.navigationTags) {
    const name = tag.match(/Name="([^"]+)"/)?.[1];
    if (!name) continue;
    navigation.push({
      name,
      relationship: tag.match(/Relationship="([^"]+)"/)?.[1] ?? "",
      toRole: tag.match(/ToRole="([^"]+)"/)?.[1] ?? "",
    });
  }
  types[type] = { entitySets: setsByType.get(type) ?? [], properties, navigation };
}

const esc = (s) => s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
const entitySetRows = entitySets
  .map(
    (s) =>
      `    EntitySet { name: "${esc(s.name)}", entity_type: "${esc(s.entityType)}", module: "${esc(
        s.module,
      )}", property_count: ${s.propertyCount}, navigation_count: ${s.navigationCount} },`,
  )
  .join("\n");

// Group endpoints by module so the assistant can name the area a request is in.
const byModule = new Map();
for (const s of entitySets) {
  if (!byModule.has(s.module)) byModule.set(s.module, []);
  byModule.get(s.module).push(s.name);
}
const moduleRows = [...byModule.entries()]
  .sort((a, b) => b[1].length - a[1].length)
  .map(
    ([module, names]) =>
      `    ("${esc(module)}", &[${names.map((n) => `"${esc(n)}"`).join(", ")}]),`,
  )
  .join("\n");
const typeBlocks = Object.entries(types)
  .map(([name, t]) => {
    const setsList = t.entitySets.map((s) => `"${esc(s)}"`).join(", ");
    const props = t.properties
      .map((p) => `            Property { name: "${esc(p.name)}", ty: "${esc(p.ty)}" },`)
      .join("\n");
    const navs = t.navigation
      .map(
        (n) =>
          `            Navigation { name: "${esc(n.name)}", relationship: "${esc(n.relationship)}", to_role: "${esc(n.toRole)}" },`,
      )
      .join("\n");
    return `    EntityType {
        name: "${esc(name)}",
        entity_sets: &[${setsList}],
        properties: &[
${props}
        ],
        navigation: &[
${navs}
        ],
    },`;
  })
  .join("\n");

const out = `// @generated by windows/scripts/gen-sap-catalogue.mjs — do not edit by hand.
//
// Source: the Service Layer $metadata of a live Business One company database.
// Regenerate against the customer's own server; the raw metadata is never
// committed. ${entitySets.length} entity sets, ${Object.keys(types).length} described types.
${missing.length ? `// Not present on this server: ${missing.join(", ")}.\n` : ""}
pub struct EntitySet {
    pub name: &'static str,
    pub entity_type: &'static str,
    /// Business One module this endpoint belongs to: \`sales\`, \`purchase\`,
    /// \`inventory\`, \`banking\`, \`crm\`, \`manufacturing\`, \`analytics\`, \`admin\`.
    pub module: &'static str,
    /// Fields this endpoint exposes. Zero means the type was not described, so
    /// only a \`$select=*\` style read is safe.
    pub property_count: u32,
    /// Foreign-key relations, the only links OData can \`$expand\`.
    pub navigation_count: u32,
}

pub struct Property {
    pub name: &'static str,
    pub ty: &'static str,
}

pub struct Navigation {
    pub name: &'static str,
    pub relationship: &'static str,
    pub to_role: &'static str,
}

pub struct EntityType {
    pub name: &'static str,
    /// Entity sets that share this type. Several sets sharing one type is the
    /// rule, not the exception: every sales and purchase document is a
    /// \`Document\`.
    pub entity_sets: &'static [&'static str],
    pub properties: &'static [Property],
    /// Foreign-key relations. These are the only links OData can \`$expand\` or
    /// \`$crossjoin\`; document-to-document links are not among them.
    pub navigation: &'static [Navigation],
}

pub const ENTITY_SETS: &[EntitySet] = &[
${entitySetRows}
];

/// Every endpoint grouped by module, largest first. This is the full surface the
/// assistant may be asked about: ${entitySets.length} entity sets.
pub const MODULES: &[(&str, &[&str])] = &[
${moduleRows}
];

/// The module an endpoint belongs to.
pub fn module_of(set: &str) -> &'static str {
    entity_set(set).map(|s| s.module).unwrap_or("unknown")
}

/// Endpoints in a module.
pub fn sets_in_module(module: &str) -> &'static [&'static str] {
    MODULES.iter().find(|(m, _)| *m == module).map(|(_, s)| *s).unwrap_or(&[])
}

pub const TYPES: &[EntityType] = &[
${typeBlocks}
];

pub fn entity_set(name: &str) -> Option<&'static EntitySet> {
    ENTITY_SETS.iter().find(|s| s.name == name)
}

pub fn entity_type(name: &str) -> Option<&'static EntityType> {
    TYPES.iter().find(|t| t.name == name)
}

/// The type behind an entity set, e.g. \`Orders\` -> \`Document\`.
pub fn type_for_set(set: &str) -> Option<&'static EntityType> {
    entity_set(set).and_then(|s| entity_type(s.entity_type))
}

/// Whether a field exists on the type behind an entity set. This is the
/// allowlist a query plan is validated against: a field the model invented is
/// rejected here instead of being sent to Business One.
pub fn has_field(set: &str, field: &str) -> bool {
    type_for_set(set).is_some_and(|t| t.properties.iter().any(|p| p.name == field))
}
`;

writeFileSync(resolve(output), out);
console.log(
  `${entitySets.length} entity sets, ${Object.keys(types).length} core types` +
    `${missing.length ? `, missing: ${missing.join(", ")}` : ""} -> ${output}`,
);
