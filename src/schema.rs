//! The org's metadata: which sObjects exist and what fields they have.
//!
//! A handful of standard objects are built in. Custom objects and custom
//! fields come in layers: `localforce.toml`, SFDX source metadata from the
//! team's repository, and whatever is created in the web UI. The catalog is
//! rebuilt from the layers whenever the UI changes the schema.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Id,
    String,
    Textarea,
    Email,
    Phone,
    Url,
    Picklist,
    Multipicklist,
    Boolean,
    Int,
    Double,
    Currency,
    Percent,
    Date,
    Datetime,
    Reference,
    /// A compound address (`BillingAddress`), read from its component fields.
    Address,
}

impl FieldType {
    /// The value of `type` in a describe.
    pub fn describe_name(self) -> &'static str {
        match self {
            FieldType::Id => "id",
            FieldType::String => "string",
            FieldType::Textarea => "textarea",
            FieldType::Email => "email",
            FieldType::Phone => "phone",
            FieldType::Url => "url",
            FieldType::Picklist => "picklist",
            FieldType::Multipicklist => "multipicklist",
            FieldType::Boolean => "boolean",
            FieldType::Int => "int",
            FieldType::Double => "double",
            FieldType::Currency => "currency",
            FieldType::Percent => "percent",
            FieldType::Date => "date",
            FieldType::Datetime => "datetime",
            FieldType::Reference => "reference",
            FieldType::Address => "address",
        }
    }

    pub fn soap_type(self) -> &'static str {
        match self {
            FieldType::Id | FieldType::Reference => "tns:ID",
            FieldType::Boolean => "xsd:boolean",
            FieldType::Int => "xsd:int",
            FieldType::Double | FieldType::Currency | FieldType::Percent => "xsd:double",
            FieldType::Date => "xsd:date",
            FieldType::Datetime => "xsd:dateTime",
            FieldType::Address => "urn:address",
            _ => "xsd:string",
        }
    }

    /// Text-like types compare case-insensitively in SOQL.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            FieldType::String
                | FieldType::Textarea
                | FieldType::Email
                | FieldType::Phone
                | FieldType::Url
                | FieldType::Picklist
                | FieldType::Multipicklist
        )
    }

    pub fn sql_type(self) -> &'static str {
        match self {
            FieldType::Boolean | FieldType::Int => "INTEGER",
            FieldType::Double | FieldType::Currency | FieldType::Percent => "REAL",
            // Ids are case-sensitive; everything else text-like is not.
            FieldType::Id | FieldType::Reference | FieldType::Date | FieldType::Datetime => "TEXT",
            _ => "TEXT COLLATE NOCASE",
        }
    }

    fn default_length(self) -> Option<u32> {
        match self {
            FieldType::Id | FieldType::Reference => Some(18),
            FieldType::String => Some(255),
            FieldType::Textarea => Some(32768),
            FieldType::Email | FieldType::Url => Some(255),
            FieldType::Phone => Some(40),
            FieldType::Picklist => Some(255),
            FieldType::Multipicklist => Some(4099),
            _ => None,
        }
    }
}

/// Where an object or field was defined. Only `Ui` definitions can be edited
/// or deleted from the web UI; the others live in files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Standard,
    Config,
    Metadata,
    Ui,
}

/// How a field gets its value when the client doesn't write it.
#[derive(Clone, Debug, PartialEq)]
pub enum Derived {
    None,
    /// Set by the platform: ids, audit fields, `IsDeleted`.
    System,
    /// `FirstName LastName`, like Contact.Name.
    FullName,
    /// An auto-number with a display format such as `INV-{0000}`.
    AutoNumber(String),
    /// A formula, evaluated when the record is read (see `formula`).
    Formula(String),
    /// A roll-up summary of a master-detail relationship's detail records.
    Rollup(Rollup),
    /// Set by the standard object's own logic, e.g. Opportunity.IsClosed from its stage.
    Logic,
    /// A compound address made of the fields starting with this prefix
    /// (`Billing` for BillingStreet, BillingCity, …; empty for Lead's Street, City, …).
    Address(String),
}

/// A roll-up summary field: COUNT, SUM, MIN or MAX over the detail records.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rollup {
    /// `count`, `sum`, `min` or `max`.
    pub op: String,
    /// The detail object, e.g. `Task_Item__c`.
    pub object: String,
    /// The detail's master-detail field, e.g. `Project__c`.
    pub via: String,
    /// The detail field summarised (not for `count`).
    pub field: Option<String>,
    #[serde(default)]
    pub filters: Vec<RollupFilter>,
}

/// Which detail records a roll-up counts: `operation` is `equals`, `notEqual`,
/// `lessThan`, `greaterThan`, `lessOrEqual`, `greaterOrEqual`, `contains` or `startsWith`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RollupFilter {
    pub field: String,
    pub operation: String,
    /// For `equals` and `notEqual`, a comma-separated list.
    pub value: String,
}

/// A validation rule: when `condition` is true, the save fails with `message`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationRule {
    pub name: String,
    /// A formula that's true when the record is invalid.
    pub condition: String,
    pub message: String,
    /// The field the error is shown on, if any.
    pub field: Option<String>,
    #[serde(default = "yes")]
    pub active: bool,
    pub description: Option<String>,
}

fn yes() -> bool {
    true
}

/// A validation rule in a custom schema layer, with the object it's on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationRuleDef {
    pub object: String,
    #[serde(flatten)]
    pub rule: ValidationRule,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub label: String,
    pub ty: FieldType,
    pub length: Option<u32>,
    pub precision: Option<u32>,
    pub scale: Option<u32>,
    pub required: bool,
    pub createable: bool,
    pub updateable: bool,
    pub custom: bool,
    pub source: Source,
    pub description: Option<String>,
    pub derived: Derived,
    pub reference_to: Option<String>,
    /// More objects a polymorphic lookup (Task.WhatId) can refer to, after `reference_to`.
    pub also_to: Vec<String>,
    pub relationship_name: Option<String>,
    pub child_relationship_name: Option<String>,
    pub cascade_delete: bool,
    pub restrict_delete: bool,
    pub master_detail: bool,
    pub picklist: Vec<String>,
    pub restricted: bool,
    pub default: Option<serde_json::Value>,
    /// A default value that's a formula (`TODAY() + 30`), evaluated when a record is created without the field.
    pub default_formula: Option<String>,
    pub external_id: bool,
    pub unique: bool,
    /// For a numeric formula: blank number fields count as zero.
    pub blank_as_zero: bool,
}

impl Field {
    fn new(name: &str, ty: FieldType) -> Self {
        Field {
            name: name.into(),
            label: label_from(name),
            ty,
            length: ty.default_length(),
            precision: None,
            scale: None,
            required: false,
            createable: true,
            updateable: true,
            custom: name.ends_with("__c"),
            source: Source::Standard,
            description: None,
            derived: Derived::None,
            reference_to: None,
            also_to: vec![],
            relationship_name: None,
            child_relationship_name: None,
            cascade_delete: false,
            restrict_delete: false,
            master_detail: false,
            picklist: vec![],
            restricted: false,
            default: None,
            default_formula: None,
            external_id: false,
            unique: false,
            blank_as_zero: true,
        }
    }
    fn required(mut self) -> Self {
        self.required = true;
        self
    }
    fn system(mut self) -> Self {
        self.derived = Derived::System;
        self.createable = false;
        self.updateable = false;
        self
    }
    fn derived(mut self, d: Derived) -> Self {
        self.derived = d;
        self.createable = false;
        self.updateable = false;
        self
    }
    fn len(mut self, n: u32) -> Self {
        self.length = Some(n);
        self
    }
    fn picklist(mut self, values: &[&str]) -> Self {
        self.picklist = values.iter().map(|v| v.to_string()).collect();
        self
    }
    fn default(mut self, v: serde_json::Value) -> Self {
        self.default = Some(v);
        self
    }
    fn lookup(mut self, to: &str, rel: &str, child: Option<&str>) -> Self {
        self.reference_to = Some(to.into());
        self.relationship_name = Some(rel.into());
        self.child_relationship_name = child.map(Into::into);
        self
    }
    fn cascade(mut self) -> Self {
        self.cascade_delete = true;
        self
    }
    /// A polymorphic lookup: it can refer to any of `to`.
    fn polymorphic(mut self, to: &[&str], rel: &str, child: Option<&str>) -> Self {
        self = self.lookup(to[0], rel, child);
        self.also_to = to[1..].iter().map(|t| t.to_string()).collect();
        self
    }
    fn logic(mut self) -> Self {
        self.derived = Derived::Logic;
        self.createable = false;
        self.updateable = false;
        self
    }
    fn fixed(mut self) -> Self {
        self.updateable = false;
        self
    }
    /// A lookup whose parent can't be deleted while this record refers to it.
    fn restrict(mut self) -> Self {
        self.restrict_delete = true;
        self
    }
    fn scaled(mut self, precision: u32, scale: u32) -> Self {
        self.precision = Some(precision);
        self.scale = Some(scale);
        self
    }

    /// Every object a lookup can refer to.
    pub fn targets(&self) -> impl Iterator<Item = &str> {
        self.reference_to.iter().chain(&self.also_to).map(String::as_str)
    }

    /// A polymorphic lookup, like Task.WhatId.
    pub fn is_polymorphic(&self) -> bool {
        !self.also_to.is_empty()
    }

    /// Formula and roll-up summary fields: computed when read, not stored.
    pub fn is_computed(&self) -> bool {
        matches!(self.derived, Derived::Formula(_) | Derived::Rollup(_) | Derived::Address(_))
    }

    /// A long text area (more than 255 characters): it can't be filtered, sorted or grouped on.
    pub fn is_long_text(&self) -> bool {
        self.ty == FieldType::Textarea && self.length.unwrap_or(32768) > 255
    }

    /// Whether SOQL can GROUP BY the field: not fractional numbers, datetimes,
    /// text areas, multi-select picklists, or formula and roll-up fields.
    pub fn groupable(&self) -> bool {
        !self.is_computed()
            && !matches!(
                self.ty,
                FieldType::Double
                    | FieldType::Currency
                    | FieldType::Percent
                    | FieldType::Datetime
                    | FieldType::Textarea
                    | FieldType::Multipicklist
            )
    }

    /// Whether SOQL can filter on the field (in WHERE).
    pub fn filterable(&self) -> bool {
        !self.is_long_text()
    }

    /// Whether SOQL can ORDER BY the field.
    pub fn sortable(&self) -> bool {
        !self.is_long_text() && self.ty != FieldType::Multipicklist
    }

    /// Whether the field can be null: not if it's required, a checkbox or
    /// maintained by the platform (Id, audit fields).
    pub fn nillable(&self) -> bool {
        !self.required && self.ty != FieldType::Boolean && self.derived != Derived::System
    }

    /// Fields the platform maintains: ids, audit fields, `IsDeleted`.
    pub fn is_system(&self) -> bool {
        self.derived == Derived::System
    }
}

#[derive(Clone, Debug)]
pub struct SObject {
    pub name: String,
    pub label: String,
    pub label_plural: String,
    pub key_prefix: String,
    pub custom: bool,
    pub source: Source,
    pub description: Option<String>,
    pub fields: Vec<Field>,
    pub validation_rules: Vec<ValidationRule>,
    field_index: HashMap<String, usize>,
    rel_index: HashMap<String, usize>,
}

impl SObject {
    fn new(name: &str, label: &str, plural: &str, prefix: &str, mut fields: Vec<Field>, owned: bool) -> Self {
        let mut all =
            vec![Field::new("Id", FieldType::Id).system(), Field::new("IsDeleted", FieldType::Boolean).system()];
        all.append(&mut fields);
        if owned {
            all.push(Field::new("OwnerId", FieldType::Reference).lookup("User", "Owner", None));
        }
        all.extend([
            Field::new("CreatedDate", FieldType::Datetime).system(),
            Field::new("CreatedById", FieldType::Reference).lookup("User", "CreatedBy", None).system(),
            Field::new("LastModifiedDate", FieldType::Datetime).system(),
            Field::new("LastModifiedById", FieldType::Reference).lookup("User", "LastModifiedBy", None).system(),
            Field::new("SystemModstamp", FieldType::Datetime).system(),
        ]);
        let mut o = SObject {
            name: name.into(),
            label: label.into(),
            label_plural: plural.into(),
            key_prefix: prefix.into(),
            custom: name.ends_with("__c"),
            source: Source::Standard,
            description: None,
            fields: all,
            validation_rules: vec![],
            field_index: HashMap::new(),
            rel_index: HashMap::new(),
        };
        o.reindex();
        o
    }

    fn reindex(&mut self) {
        self.field_index = self.fields.iter().enumerate().map(|(i, f)| (f.name.to_ascii_lowercase(), i)).collect();
        self.rel_index = self
            .fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.relationship_name.as_ref().map(|r| (r.to_ascii_lowercase(), i)))
            .collect();
    }

    /// Case-insensitive field lookup, as Salesforce does.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.field_index.get(&name.to_ascii_lowercase()).map(|i| &self.fields[*i])
    }

    /// The reference field behind a parent relationship name (`Account`, `Owner`, `Foo__r`).
    pub fn relationship(&self, name: &str) -> Option<&Field> {
        self.rel_index.get(&name.to_ascii_lowercase()).map(|i| &self.fields[*i])
    }

    /// The field that names a record: `Name`, or `CaseNumber` on Case.
    pub fn name_field(&self) -> Option<&Field> {
        self.field("Name").or_else(|| self.field("CaseNumber"))
    }
}

/// A parent-to-child relationship: the parent's `Contacts` is Contact.AccountId.
#[derive(Clone, Debug)]
pub struct ChildRelationship {
    pub name: String,
    pub child_object: String,
    pub field: String,
    pub cascade_delete: bool,
    pub restrict_delete: bool,
}

#[derive(Debug)]
pub struct Catalog {
    pub objects: Vec<SObject>,
    index: HashMap<String, usize>,
    children: HashMap<String, Vec<ChildRelationship>>,
    by_prefix: HashMap<String, usize>,
    /// The `Name` object that polymorphic lookups (`What.Name`) lead to: the
    /// shared fields of every object they can refer to. It isn't queryable
    /// itself; it's read through the `_Name` view (see `store::sync_tables`).
    pub name_object: SObject,
}

impl Catalog {
    pub fn object(&self, name: &str) -> Option<&SObject> {
        self.index.get(&name.to_ascii_lowercase()).map(|i| &self.objects[*i])
    }

    pub fn object_for_id(&self, id: &str) -> Option<&SObject> {
        self.by_prefix.get(id.get(..3)?).map(|i| &self.objects[*i])
    }

    pub fn children_of(&self, object: &str) -> &[ChildRelationship] {
        self.children.get(&object.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn child_relationship(&self, parent: &str, name: &str) -> Option<&ChildRelationship> {
        self.children_of(parent).iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// The object a lookup leads to: its target, or `Name` for a polymorphic lookup.
    pub fn parent_of(&self, f: &Field) -> Option<&SObject> {
        if f.is_polymorphic() { Some(&self.name_object) } else { self.object(f.reference_to.as_deref()?) }
    }

    /// A catalog of just these objects, e.g. the Tooling API's.
    pub fn of(objects: Vec<SObject>) -> Catalog {
        let index = objects.iter().enumerate().map(|(i, o)| (o.name.to_ascii_lowercase(), i)).collect();
        let by_prefix = objects.iter().enumerate().map(|(i, o)| (o.key_prefix.clone(), i)).collect();
        Catalog { objects, index, children: HashMap::new(), by_prefix, name_object: name_object() }
    }

    /// Build the catalog from the standard objects plus each layer of custom
    /// definitions. All objects are created before any `[[fields]]` are
    /// added, so a layer can add fields to an object from another layer.
    pub fn build(layers: &[(Source, &CustomSchema)]) -> anyhow::Result<Catalog> {
        let mut objects = standard_objects();
        let mut taken: Vec<String> = objects.iter().map(|o| o.key_prefix.clone()).collect();

        let mut defs: Vec<(Source, &CustomObjectDef)> =
            layers.iter().flat_map(|(src, l)| l.objects.iter().map(move |o| (*src, o))).collect();
        // Explicit prefixes first, then derived ones in name order, so derived
        // prefixes don't depend on the order files were read in.
        defs.sort_by_key(|(_, d)| (d.key_prefix.is_none(), d.name.to_ascii_lowercase()));
        for (src, def) in defs {
            validate_custom_name(&def.name, "object")?;
            if let Some(existing) = objects.iter().find(|o| o.name.eq_ignore_ascii_case(&def.name)) {
                anyhow::bail!(
                    "object {} is defined in both {} and {}",
                    def.name,
                    source_label(existing.source),
                    source_label(src)
                );
            }
            let prefix = match &def.key_prefix {
                Some(p) => p.clone(),
                None => derive_prefix(&def.name, &taken),
            };
            anyhow::ensure!(!taken.contains(&prefix), "key prefix {prefix} of {} is already used", def.name);
            taken.push(prefix.clone());
            let label = def.label.clone().unwrap_or_else(|| label_from(&def.name));
            let plural = def.label_plural.clone().unwrap_or_else(|| format!("{label}s"));
            let mut name = Field::new("Name", FieldType::String).len(80);
            name.label = def.name_field_label.clone().unwrap_or_else(|| format!("{label} Name"));
            name.source = src;
            match &def.name_field_format {
                Some(fmt) => name.derived = Derived::AutoNumber(fmt.clone()),
                None => name.required = true,
            }
            if matches!(name.derived, Derived::AutoNumber(_)) {
                name.createable = false;
                name.updateable = false;
            }
            let mut fields = vec![name];
            for f in &def.fields {
                let mut field = f.to_field()?;
                field.source = src;
                fields.push(field);
            }
            let mut o = SObject::new(&def.name, &label, &plural, &prefix, fields, true);
            o.source = src;
            o.description = def.description.clone();
            for f in &mut o.fields {
                if f.is_system() || f.name == "OwnerId" {
                    f.source = Source::Standard;
                }
                if f.name == "OwnerId" {
                    f.required = true;
                }
            }
            objects.push(o);
        }
        for (src, layer) in layers {
            for def in &layer.fields {
                let obj = objects.iter_mut().find(|o| o.name.eq_ignore_ascii_case(&def.object)).ok_or_else(|| {
                    anyhow::anyhow!("custom field {} is on unknown object {}", def.field.name, def.object)
                })?;
                if let Some(existing) = obj.field(&def.field.name) {
                    anyhow::bail!(
                        "field {}.{} is defined in both {} and {}",
                        obj.name,
                        existing.name,
                        source_label(existing.source),
                        source_label(*src)
                    );
                }
                let mut field = def.field.to_field()?;
                field.source = *src;
                let at = obj.fields.len() - 5; // before the audit fields
                obj.fields.insert(at, field);
                obj.reindex();
            }
        }

        // Detail objects (the child side of a master-detail) take their owner
        // from the master and have no OwnerId of their own.
        for o in objects.iter_mut().filter(|o| o.custom && o.fields.iter().any(|f| f.master_detail)) {
            o.fields.retain(|f| f.name != "OwnerId");
            o.reindex();
        }

        // Tasks and events can be about any custom object.
        let custom: Vec<String> = objects.iter().filter(|o| o.custom).map(|o| o.name.clone()).collect();
        for o in objects.iter_mut().filter(|o| matches!(o.name.as_str(), "Task" | "Event")) {
            for f in o.fields.iter_mut().filter(|f| f.name == "WhatId") {
                f.also_to.extend(custom.iter().cloned());
            }
        }

        for (src, layer) in layers {
            for def in &layer.validation_rules {
                let obj = objects.iter_mut().find(|o| o.name.eq_ignore_ascii_case(&def.object)).ok_or_else(|| {
                    anyhow::anyhow!("validation rule {} is on unknown object {}", def.rule.name, def.object)
                })?;
                anyhow::ensure!(
                    !obj.validation_rules.iter().any(|r| r.name.eq_ignore_ascii_case(&def.rule.name)),
                    "validation rule {}.{} is defined twice (the second time in {})",
                    obj.name,
                    def.rule.name,
                    source_label(*src)
                );
                obj.validation_rules.push(def.rule.clone());
            }
        }

        let index = objects.iter().enumerate().map(|(i, o)| (o.name.to_ascii_lowercase(), i)).collect();
        let by_prefix = objects.iter().enumerate().map(|(i, o)| (o.key_prefix.clone(), i)).collect();
        let mut children: HashMap<String, Vec<ChildRelationship>> = HashMap::new();
        for o in &objects {
            for f in &o.fields {
                let Some(rel) = &f.child_relationship_name else { continue };
                for to in f.targets() {
                    children.entry(to.to_ascii_lowercase()).or_default().push(ChildRelationship {
                        name: rel.clone(),
                        child_object: o.name.clone(),
                        field: f.name.clone(),
                        cascade_delete: f.cascade_delete,
                        restrict_delete: f.restrict_delete,
                    });
                }
            }
        }
        let cat = Catalog { objects, index, children, by_prefix, name_object: name_object() };
        for o in &cat.objects {
            let mut rels = std::collections::HashSet::new();
            for f in &o.fields {
                for to in f.targets() {
                    anyhow::ensure!(cat.object(to).is_some(), "{}.{} references unknown object {to}", o.name, f.name);
                }
                if let Some(r) = &f.relationship_name {
                    anyhow::ensure!(
                        rels.insert(r.to_ascii_lowercase()),
                        "{}: relationship name {r} is used twice",
                        o.name
                    );
                }
            }
            let mut child_names = std::collections::HashSet::new();
            for c in cat.children_of(&o.name) {
                anyhow::ensure!(
                    child_names.insert(c.name.to_ascii_lowercase()),
                    "{}: child relationship name {} is used twice",
                    o.name,
                    c.name
                );
            }
        }
        Ok(cat)
    }
}

pub fn source_label(s: Source) -> &'static str {
    match s {
        Source::Standard => "the standard schema",
        Source::Config => "localforce.toml",
        Source::Metadata => "SFDX metadata",
        Source::Ui => "the web UI",
    }
}

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A key prefix for a custom object, `a` plus two base-62 characters, derived
/// from its name so it stays the same across restarts.
pub fn derive_prefix(name: &str, taken: &[String]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.to_ascii_lowercase().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    let mut n = (h % (62 * 62)) as usize;
    loop {
        let p = format!("a{}{}", BASE62_CHARS[n / 62] as char, BASE62_CHARS[n % 62] as char);
        if !taken.contains(&p) {
            return p;
        }
        n = (n + 1) % (62 * 62);
    }
}

/// API names: a letter, then letters, digits and single underscores, then `__c`.
pub fn validate_custom_name(name: &str, what: &str) -> anyhow::Result<()> {
    let base = name.strip_suffix("__c").ok_or_else(|| anyhow::anyhow!("custom {what} name {name} must end in __c"))?;
    let ok = base.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && base.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !base.contains("__")
        && !base.ends_with('_')
        && base.len() <= 40;
    anyhow::ensure!(
        ok,
        "{name} isn't a valid API name: use a letter, then up to 40 letters, digits and single underscores, then __c"
    );
    Ok(())
}

fn label_from(api_name: &str) -> String {
    let base = api_name.trim_end_matches("__c").trim_end_matches("Id").replace('_', " ");
    let mut out = String::new();
    for (i, c) in base.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push(c);
    }
    if out.is_empty() { api_name.to_string() } else { out }
}

/// Render an auto-number display format such as `INV-{0000}` or `{YYYY}-{0}`.
pub fn format_auto_number(format: &str, n: i64, now: chrono::DateTime<chrono::Utc>) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(start) = rest.find('{') {
        out += &rest[..start];
        let Some(end) = rest[start..].find('}') else { break };
        let token = &rest[start + 1..start + end];
        match token {
            "YYYY" => out += &now.format("%Y").to_string(),
            "YY" => out += &now.format("%y").to_string(),
            "MM" => out += &now.format("%m").to_string(),
            "DD" => out += &now.format("%d").to_string(),
            t if !t.is_empty() && t.chars().all(|c| c == '0') => out += &format!("{n:0width$}", width = t.len()),
            t => {
                out.push('{');
                out += t;
                out.push('}');
            }
        }
        rest = &rest[start + end + 1..];
    }
    out + rest
}

fn standard_objects() -> Vec<SObject> {
    use FieldType::*;
    let s = |n: &str| Field::new(n, String);
    let pick = |n: &str, v: &[&str]| Field::new(n, Picklist).picklist(v);
    vec![
        SObject::new(
            "User",
            "User",
            "Users",
            "005",
            vec![
                Field::new("Username", Email).required(),
                s("FirstName").len(40),
                s("LastName").len(80).required(),
                s("Name").len(121).derived(Derived::FullName),
                Field::new("Email", Email).required(),
                s("Alias").len(8).required(),
                Field::new("IsActive", Boolean).default(true.into()),
                s("Title").len(80),
            ],
            false,
        ),
        SObject::new(
            "Account",
            "Account",
            "Accounts",
            "001",
            vec![
                s("Name").required(),
                pick(
                    "Type",
                    &[
                        "Prospect",
                        "Customer - Direct",
                        "Customer - Channel",
                        "Channel Partner / Reseller",
                        "Installation Partner",
                        "Technology Partner",
                        "Other",
                    ],
                ),
                pick("Industry", INDUSTRIES),
                Field::new("AnnualRevenue", Currency),
                Field::new("NumberOfEmployees", Int),
                Field::new("Website", Url),
                Field::new("Phone", Phone),
                s("BillingCity").len(40),
                s("BillingCountry").len(80),
                Field::new("ParentId", Reference).lookup("Account", "Parent", Some("ChildAccounts")),
                Field::new("MasterRecordId", Reference).lookup("Account", "MasterRecord", None).logic(),
                Field::new("BillingStreet", Textarea).len(255),
                s("BillingState").len(80),
                s("BillingPostalCode").len(20),
                Field::new("BillingLatitude", Double).scaled(18, 15),
                Field::new("BillingLongitude", Double).scaled(18, 15),
                pick(
                    "BillingGeocodeAccuracy",
                    &[
                        "Address",
                        "NearAddress",
                        "Block",
                        "Street",
                        "ExtendedZip",
                        "Zip",
                        "Neighborhood",
                        "City",
                        "County",
                        "State",
                        "Unknown",
                    ],
                ),
                Field::new("BillingAddress", Address).derived(Derived::Address("Billing".into())),
                Field::new("ShippingStreet", Textarea).len(255),
                s("ShippingCity").len(40),
                s("ShippingState").len(80),
                s("ShippingPostalCode").len(20),
                s("ShippingCountry").len(80),
                Field::new("ShippingLatitude", Double).scaled(18, 15),
                Field::new("ShippingLongitude", Double).scaled(18, 15),
                pick(
                    "ShippingGeocodeAccuracy",
                    &[
                        "Address",
                        "NearAddress",
                        "Block",
                        "Street",
                        "ExtendedZip",
                        "Zip",
                        "Neighborhood",
                        "City",
                        "County",
                        "State",
                        "Unknown",
                    ],
                ),
                Field::new("ShippingAddress", Address).derived(Derived::Address("Shipping".into())),
                Field::new("Fax", Phone).len(40),
                s("AccountNumber").len(40),
                Field::new("PhotoUrl", Url).len(255).logic(),
                s("Sic").len(20),
                pick("Ownership", &["Public", "Private", "Subsidiary", "Other"]),
                s("TickerSymbol").len(20),
                Field::new("Description", Textarea).len(32000),
                pick("Rating", &["Hot", "Warm", "Cold"]),
                s("Site").len(80),
                Field::new("LastActivityDate", Date).logic(),
                Field::new("LastViewedDate", Datetime).logic(),
                Field::new("LastReferencedDate", Datetime).logic(),
                pick("AccountSource", &["Web", "Phone Inquiry", "Partner Referral", "Purchased List", "Other"]),
            ],
            true,
        ),
        SObject::new(
            "Contact",
            "Contact",
            "Contacts",
            "003",
            vec![
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Contacts")).cascade(),
                s("FirstName").len(40),
                s("LastName").len(80).required(),
                s("Name").len(121).derived(Derived::FullName),
                Field::new("Email", Email),
                Field::new("Phone", Phone),
                s("Title").len(128),
                Field::new("Birthdate", Date),
                Field::new("HasOptedOutOfEmail", Boolean),
                Field::new("MasterRecordId", Reference).lookup("Contact", "MasterRecord", None).logic(),
                pick("Salutation", &["Mr.", "Ms.", "Mrs.", "Dr.", "Prof.", "Mx."]),
                Field::new("OtherStreet", Textarea).len(255),
                s("OtherCity").len(40),
                s("OtherState").len(80),
                s("OtherPostalCode").len(20),
                s("OtherCountry").len(80),
                Field::new("OtherLatitude", Double).scaled(18, 15),
                Field::new("OtherLongitude", Double).scaled(18, 15),
                pick(
                    "OtherGeocodeAccuracy",
                    &[
                        "Address",
                        "NearAddress",
                        "Block",
                        "Street",
                        "ExtendedZip",
                        "Zip",
                        "Neighborhood",
                        "City",
                        "County",
                        "State",
                        "Unknown",
                    ],
                ),
                Field::new("OtherAddress", Address).derived(Derived::Address("Other".into())),
                Field::new("MailingStreet", Textarea).len(255),
                s("MailingCity").len(40),
                s("MailingState").len(80),
                s("MailingPostalCode").len(20),
                s("MailingCountry").len(80),
                Field::new("MailingLatitude", Double).scaled(18, 15),
                Field::new("MailingLongitude", Double).scaled(18, 15),
                pick(
                    "MailingGeocodeAccuracy",
                    &[
                        "Address",
                        "NearAddress",
                        "Block",
                        "Street",
                        "ExtendedZip",
                        "Zip",
                        "Neighborhood",
                        "City",
                        "County",
                        "State",
                        "Unknown",
                    ],
                ),
                Field::new("MailingAddress", Address).derived(Derived::Address("Mailing".into())),
                Field::new("Fax", Phone).len(40),
                Field::new("MobilePhone", Phone).len(40),
                Field::new("HomePhone", Phone).len(40),
                Field::new("OtherPhone", Phone).len(40),
                Field::new("AssistantPhone", Phone).len(40),
                Field::new("ReportsToId", Reference).lookup("Contact", "ReportsTo", None),
                s("Department").len(80),
                s("AssistantName").len(40),
                pick("LeadSource", &["Web", "Phone Inquiry", "Partner Referral", "Purchased List", "Other"]),
                Field::new("Description", Textarea).len(32000),
                Field::new("HasOptedOutOfFax", Boolean),
                Field::new("DoNotCall", Boolean),
                Field::new("LastActivityDate", Date).logic(),
                Field::new("LastCURequestDate", Datetime).logic(),
                Field::new("LastCUUpdateDate", Datetime).logic(),
                Field::new("LastViewedDate", Datetime).logic(),
                Field::new("LastReferencedDate", Datetime).logic(),
                s("EmailBouncedReason").len(255),
                Field::new("EmailBouncedDate", Datetime),
                Field::new("IsEmailBounced", Boolean).logic(),
                Field::new("PhotoUrl", Url).len(255).logic(),
            ],
            true,
        ),
        SObject::new(
            "Lead",
            "Lead",
            "Leads",
            "00Q",
            vec![
                s("FirstName").len(40),
                s("LastName").len(80).required(),
                s("Name").len(121).derived(Derived::FullName),
                s("Company").required(),
                Field::new("Email", Email),
                pick(
                    "Status",
                    &["Open - Not Contacted", "Working - Contacted", "Closed - Converted", "Closed - Not Converted"],
                )
                .required()
                .default("Open - Not Contacted".into()),
                pick("LeadSource", &["Web", "Phone Inquiry", "Partner Referral", "Purchased List", "Other"]),
                Field::new("IsConverted", Boolean).fixed(),
                Field::new("MasterRecordId", Reference).lookup("Lead", "MasterRecord", None).logic(),
                pick("Salutation", &["Mr.", "Ms.", "Mrs.", "Dr.", "Prof.", "Mx."]),
                s("Title").len(128),
                Field::new("Street", Textarea).len(255),
                s("City").len(40),
                s("State").len(80),
                s("PostalCode").len(20),
                s("Country").len(80),
                Field::new("Latitude", Double).scaled(18, 15),
                Field::new("Longitude", Double).scaled(18, 15),
                pick(
                    "GeocodeAccuracy",
                    &[
                        "Address",
                        "NearAddress",
                        "Block",
                        "Street",
                        "ExtendedZip",
                        "Zip",
                        "Neighborhood",
                        "City",
                        "County",
                        "State",
                        "Unknown",
                    ],
                ),
                Field::new("Address", Address).derived(Derived::Address("".into())),
                Field::new("Phone", Phone).len(40),
                Field::new("MobilePhone", Phone).len(40),
                Field::new("Fax", Phone).len(40),
                Field::new("Website", Url).len(255),
                Field::new("PhotoUrl", Url).len(255).logic(),
                Field::new("Description", Textarea).len(32000),
                pick("Industry", INDUSTRIES),
                pick("Rating", &["Hot", "Warm", "Cold"]),
                Field::new("AnnualRevenue", Currency).scaled(18, 0),
                Field::new("NumberOfEmployees", Int),
                Field::new("HasOptedOutOfEmail", Boolean),
                Field::new("HasOptedOutOfFax", Boolean),
                Field::new("DoNotCall", Boolean),
                Field::new("ConvertedDate", Date).logic(),
                Field::new("ConvertedAccountId", Reference).lookup("Account", "ConvertedAccount", None).logic(),
                Field::new("ConvertedContactId", Reference).lookup("Contact", "ConvertedContact", None).logic(),
                Field::new("ConvertedOpportunityId", Reference)
                    .lookup("Opportunity", "ConvertedOpportunity", None)
                    .logic(),
                Field::new("IsUnreadByOwner", Boolean),
                Field::new("LastActivityDate", Date).logic(),
                Field::new("LastTransferDate", Date).logic(),
                Field::new("LastViewedDate", Datetime).logic(),
                Field::new("LastReferencedDate", Datetime).logic(),
                s("EmailBouncedReason").len(255),
                Field::new("EmailBouncedDate", Datetime),
            ],
            true,
        ),
        SObject::new(
            "Opportunity",
            "Opportunity",
            "Opportunities",
            "006",
            vec![
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Opportunities")).cascade(),
                s("Name").len(120).required(),
                pick("StageName", &OPPORTUNITY_STAGES.iter().map(|s| s.name).collect::<Vec<_>>()).required(),
                Field::new("Amount", Currency),
                Field::new("Probability", Percent),
                Field::new("CloseDate", Date).required(),
                pick(
                    "Type",
                    &[
                        "Existing Customer - Upgrade",
                        "Existing Customer - Replacement",
                        "Existing Customer - Downgrade",
                        "New Customer",
                    ],
                ),
                pick("LeadSource", &["Web", "Phone Inquiry", "Partner Referral", "Purchased List", "Other"]),
                s("NextStep"),
                Field::new("Description", Textarea),
                pick("ForecastCategoryName", &["Omitted", "Pipeline", "Best Case", "Commit", "Closed"]),
                Field::new("IsClosed", Boolean).logic(),
                Field::new("IsWon", Boolean).logic(),
                Field::new("Pricebook2Id", Reference).lookup("Pricebook2", "Pricebook2", Some("Opportunities")),
                Field::new("CampaignId", Reference).lookup("Campaign", "Campaign", Some("Opportunities")),
                Field::new("ExpectedRevenue", Currency).logic(),
                Field::new("HasOpportunityLineItem", Boolean).logic(),
                Field::new("TotalOpportunityQuantity", Double),
                Field::new("IsPrivate", Boolean),
                pick("ForecastCategory", &["Omitted", "Pipeline", "BestCase", "MostLikely", "Forecast", "Closed"])
                    .logic(),
                Field::new("LastActivityDate", Date).logic(),
                Field::new("PushCount", Int).logic(),
                Field::new("LastStageChangeDate", Datetime).logic(),
                Field::new("FiscalQuarter", Int).logic(),
                Field::new("FiscalYear", Int).logic(),
                s("Fiscal").len(6).logic(),
                Field::new("LastViewedDate", Datetime).logic(),
                Field::new("LastReferencedDate", Datetime).logic(),
                Field::new("HasOpenActivity", Boolean).logic(),
                Field::new("HasOverdueTask", Boolean).logic(),
            ],
            true,
        ),
        SObject::new(
            "Case",
            "Case",
            "Cases",
            "500",
            vec![
                s("CaseNumber").len(30).derived(Derived::AutoNumber("{00000000}".into())),
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Cases")).cascade(),
                Field::new("ContactId", Reference).lookup("Contact", "Contact", Some("Cases")),
                s("Subject"),
                Field::new("Description", Textarea),
                pick("Status", &["New", "Working", "Escalated", "Closed"]).default("New".into()),
                pick("Priority", &["High", "Medium", "Low"]).default("Medium".into()),
                pick("Origin", &["Phone", "Email", "Web"]),
                Field::new("IsClosed", Boolean).logic(),
                Field::new("ClosedDate", Datetime).logic(),
                Field::new("MasterRecordId", Reference).lookup("Case", "MasterRecord", None).logic(),
                Field::new("AssetId", Reference).lookup("Asset", "Asset", Some("Cases")),
                Field::new("ParentId", Reference).lookup("Case", "Parent", Some("Cases")),
                s("SuppliedName").len(80),
                Field::new("SuppliedEmail", Email).len(80),
                s("SuppliedPhone").len(40),
                s("SuppliedCompany").len(80),
                pick("Type", &["Mechanical", "Electrical", "Electronic", "Structural", "Other"]),
                pick(
                    "Reason",
                    &[
                        "Installation",
                        "Equipment Complexity",
                        "Performance",
                        "Breakdown",
                        "Equipment Design",
                        "Feedback",
                        "Other",
                    ],
                ),
                Field::new("IsEscalated", Boolean),
                Field::new("ContactPhone", Phone).len(40).logic(),
                Field::new("ContactMobile", Phone).len(40).logic(),
                Field::new("ContactEmail", Email).len(80).logic(),
                Field::new("ContactFax", Phone).len(40).logic(),
                Field::new("Comments", Textarea).len(4000),
                Field::new("LastViewedDate", Datetime).logic(),
                Field::new("LastReferencedDate", Datetime).logic(),
            ],
            true,
        ),
        SObject::new(
            "Task",
            "Task",
            "Tasks",
            "00T",
            vec![
                Field::new("WhoId", Reference).polymorphic(&["Contact", "Lead"], "Who", Some("Tasks")),
                Field::new("WhatId", Reference).polymorphic(&["Account", "Opportunity", "Case"], "What", Some("Tasks")),
                Field::new("AccountId", Reference).lookup("Account", "Account", None).logic(),
                s("Subject"),
                Field::new("ActivityDate", Date),
                pick("Status", &["Not Started", "In Progress", "Completed", "Waiting on someone else", "Deferred"])
                    .required()
                    .default("Not Started".into()),
                pick("Priority", &["High", "Normal", "Low"]).required().default("Normal".into()),
                pick("Type", &["Call", "Email", "Meeting", "Other"]),
                pick("TaskSubtype", &["Task", "Email", "ListEmail", "Cadence", "Call", "LinkedIn"]),
                Field::new("Description", Textarea),
                Field::new("IsClosed", Boolean).logic(),
                Field::new("IsHighPriority", Boolean).logic(),
                Field::new("CompletedDateTime", Datetime).logic(),
            ],
            true,
        ),
        SObject::new(
            "Event",
            "Event",
            "Events",
            "00U",
            vec![
                Field::new("WhoId", Reference).polymorphic(&["Contact", "Lead"], "Who", Some("Events")),
                Field::new("WhatId", Reference).polymorphic(
                    &["Account", "Opportunity", "Case"],
                    "What",
                    Some("Events"),
                ),
                Field::new("AccountId", Reference).lookup("Account", "Account", None).logic(),
                s("Subject"),
                s("Location"),
                Field::new("StartDateTime", Datetime),
                Field::new("EndDateTime", Datetime),
                Field::new("ActivityDateTime", Datetime),
                Field::new("ActivityDate", Date).logic(),
                Field::new("DurationInMinutes", Int),
                Field::new("IsAllDayEvent", Boolean),
                pick("ShowAs", &["Busy", "OutOfOffice", "Free"]).default("Busy".into()),
                pick("Type", &["Call", "Email", "Meeting", "Other"]),
                Field::new("Description", Textarea),
            ],
            true,
        ),
        SObject::new(
            "Product2",
            "Product",
            "Products",
            "01t",
            vec![
                s("Name").required(),
                s("ProductCode"),
                Field::new("Description", Textarea).len(4000),
                Field::new("IsActive", Boolean),
                pick("Family", &["None"]),
                s("StockKeepingUnit").len(180),
            ],
            false,
        ),
        SObject::new(
            "Pricebook2",
            "Price Book",
            "Price Books",
            "01s",
            vec![
                s("Name").required(),
                Field::new("IsActive", Boolean),
                Field::new("IsStandard", Boolean).logic(),
                s("Description"),
            ],
            false,
        ),
        SObject::new(
            "PricebookEntry",
            "Price Book Entry",
            "Price Book Entries",
            "01u",
            vec![
                s("Name").logic(),
                Field::new("Pricebook2Id", Reference)
                    .lookup("Pricebook2", "Pricebook2", Some("PricebookEntries"))
                    .required()
                    .fixed()
                    .cascade(),
                Field::new("Product2Id", Reference)
                    .lookup("Product2", "Product2", Some("PricebookEntries"))
                    .required()
                    .fixed()
                    .cascade(),
                Field::new("UnitPrice", Currency).required(),
                Field::new("IsActive", Boolean),
                Field::new("UseStandardPrice", Boolean),
                s("ProductCode").logic(),
            ],
            false,
        ),
        SObject::new(
            "OpportunityLineItem",
            "Opportunity Product",
            "Opportunity Product",
            "00k",
            vec![
                Field::new("OpportunityId", Reference)
                    .lookup("Opportunity", "Opportunity", Some("OpportunityLineItems"))
                    .required()
                    .fixed()
                    .cascade(),
                // Required, with Salesforce's own error when it's missing (see `sales`).
                Field::new("PricebookEntryId", Reference)
                    .lookup("PricebookEntry", "PricebookEntry", Some("OpportunityLineItems"))
                    .fixed()
                    .restrict(),
                Field::new("Product2Id", Reference).lookup("Product2", "Product2", None).logic(),
                s("Name").len(376).logic(),
                s("ProductCode").logic(),
                Field::new("Quantity", Double).scaled(12, 2).required(),
                Field::new("UnitPrice", Currency).scaled(18, 2),
                Field::new("ListPrice", Currency).scaled(18, 2).logic(),
                Field::new("Subtotal", Currency).scaled(18, 2).logic(),
                Field::new("TotalPrice", Currency).scaled(18, 2),
                Field::new("Discount", Percent).scaled(5, 2),
                Field::new("ServiceDate", Date),
                s("Description"),
                Field::new("SortOrder", Int),
            ],
            false,
        ),
        SObject::new(
            "OpportunityContactRole",
            "Opportunity Contact Role",
            "Opportunity Contact Role",
            "00K",
            vec![
                Field::new("OpportunityId", Reference)
                    .lookup("Opportunity", "Opportunity", Some("OpportunityContactRoles"))
                    .required()
                    .fixed()
                    .cascade(),
                Field::new("ContactId", Reference)
                    .lookup("Contact", "Contact", Some("OpportunityContactRoles"))
                    .required()
                    .cascade(),
                pick(
                    "Role",
                    &[
                        "Business User",
                        "Decision Maker",
                        "Economic Buyer",
                        "Economic Decision Maker",
                        "Evaluator",
                        "Executive Sponsor",
                        "Influencer",
                        "Technical Buyer",
                        "Other",
                    ],
                ),
                Field::new("IsPrimary", Boolean),
            ],
            false,
        ),
        SObject::new(
            "Campaign",
            "Campaign",
            "Campaigns",
            "701",
            vec![
                s("Name").len(80).required(),
                pick(
                    "Type",
                    &[
                        "Conference",
                        "Webinar",
                        "Trade Show",
                        "Public Relations",
                        "Partners",
                        "Referral Program",
                        "Advertisement",
                        "Banner Ads",
                        "Direct Mail",
                        "Email",
                        "Telemarketing",
                        "Other",
                    ],
                ),
                pick("Status", &["Planned", "In Progress", "Completed", "Aborted"]),
                Field::new("StartDate", Date),
                Field::new("EndDate", Date),
                Field::new("IsActive", Boolean),
                Field::new("BudgetedCost", Currency),
                Field::new("ActualCost", Currency),
                Field::new("ExpectedRevenue", Currency),
                Field::new("ExpectedResponse", Percent),
                Field::new("NumberSent", Double),
                Field::new("Description", Textarea).len(32000),
                Field::new("ParentId", Reference).lookup("Campaign", "Parent", Some("ChildCampaigns")),
                Field::new("NumberOfLeads", Int).logic(),
                Field::new("NumberOfContacts", Int).logic(),
                Field::new("NumberOfResponses", Int).logic(),
                Field::new("NumberOfOpportunities", Int).logic(),
                Field::new("NumberOfWonOpportunities", Int).logic(),
                Field::new("AmountAllOpportunities", Currency).logic(),
                Field::new("AmountWonOpportunities", Currency).logic(),
            ],
            true,
        ),
        SObject::new(
            "CampaignMember",
            "Campaign Member",
            "Campaign Members",
            "00v",
            vec![
                Field::new("CampaignId", Reference)
                    .lookup("Campaign", "Campaign", Some("CampaignMembers"))
                    .required()
                    .fixed()
                    .cascade(),
                Field::new("LeadId", Reference).lookup("Lead", "Lead", Some("CampaignMembers")).fixed().cascade(),
                Field::new("ContactId", Reference)
                    .lookup("Contact", "Contact", Some("CampaignMembers"))
                    .fixed()
                    .cascade(),
                pick("Status", &["Planned", "Received", "Responded", "Sent"]).default("Sent".into()),
                Field::new("HasResponded", Boolean).logic(),
                Field::new("FirstRespondedDate", Date).logic(),
                s("FirstName").len(40).logic(),
                s("LastName").len(80).logic(),
                s("Name").len(121).logic(),
                Field::new("Email", Email).logic(),
            ],
            false,
        ),
        SObject::new(
            "Contract",
            "Contract",
            "Contracts",
            "800",
            vec![
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Contracts")).required().cascade(),
                s("ContractNumber").len(30).derived(Derived::AutoNumber("{00000000}".into())),
                pick("Status", &["Draft", "In Approval Process", "Activated"]).required().default("Draft".into()),
                pick("StatusCode", &["Draft", "InApproval", "Activated", "Terminated", "Expired"]).logic(),
                Field::new("StartDate", Date),
                Field::new("ContractTerm", Int),
                Field::new("EndDate", Date).logic(),
                Field::new("ActivatedDate", Datetime).logic(),
                Field::new("Description", Textarea),
                Field::new("SpecialTerms", Textarea).len(4000),
            ],
            true,
        ),
        SObject::new(
            "Order",
            "Order",
            "Orders",
            "801",
            vec![
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Orders")).required().cascade(),
                Field::new("ContractId", Reference).lookup("Contract", "Contract", Some("Orders")),
                Field::new("Pricebook2Id", Reference).lookup("Pricebook2", "Pricebook2", Some("Orders")),
                s("Name").len(80),
                s("OrderNumber").len(30).derived(Derived::AutoNumber("{00000000}".into())),
                Field::new("EffectiveDate", Date).required(),
                Field::new("EndDate", Date),
                pick("Status", &["Draft", "Activated"]).required().default("Draft".into()),
                pick("StatusCode", &["Draft", "Activated", "Cancelled", "Expired"]).logic(),
                pick("Type", &["New", "Renewal", "Upgrade"]),
                Field::new("Description", Textarea).len(32000),
                Field::new("TotalAmount", Currency).logic(),
                Field::new("ActivatedDate", Datetime).logic(),
            ],
            true,
        ),
        SObject::new(
            "OrderItem",
            "Order Product",
            "Order Products",
            "802",
            vec![
                Field::new("OrderId", Reference)
                    .lookup("Order", "Order", Some("OrderItems"))
                    .required()
                    .fixed()
                    .cascade(),
                Field::new("PricebookEntryId", Reference)
                    .lookup("PricebookEntry", "PricebookEntry", Some("OrderItems"))
                    .fixed()
                    .restrict(),
                Field::new("Product2Id", Reference).lookup("Product2", "Product2", None).fixed(),
                s("OrderItemNumber").len(30).derived(Derived::AutoNumber("{0000000000}".into())),
                Field::new("Quantity", Double).scaled(12, 2).required(),
                Field::new("UnitPrice", Currency).scaled(18, 2).required(),
                Field::new("ListPrice", Currency).scaled(18, 2).logic(),
                Field::new("TotalPrice", Currency).scaled(18, 2).logic(),
                Field::new("ServiceDate", Date),
                Field::new("EndDate", Date),
                s("Description"),
            ],
            false,
        ),
        SObject::new(
            "Asset",
            "Asset",
            "Assets",
            "02i",
            vec![
                s("Name").required(),
                Field::new("AccountId", Reference).lookup("Account", "Account", Some("Assets")).cascade(),
                Field::new("ContactId", Reference).lookup("Contact", "Contact", Some("Assets")),
                Field::new("Product2Id", Reference).lookup("Product2", "Product2", Some("Assets")),
                s("SerialNumber").len(80),
                pick("Status", &["Purchased", "Shipped", "Installed", "Registered", "Obsolete"]),
                Field::new("Quantity", Double),
                Field::new("Price", Currency),
                Field::new("PurchaseDate", Date),
                Field::new("InstallDate", Date),
                Field::new("UsageEndDate", Date),
                Field::new("IsCompetitorProduct", Boolean),
                Field::new("Description", Textarea).len(32000),
            ],
            true,
        ),
    ]
    .into_iter()
    .map(|mut o| {
        // Picklists on standard objects are unrestricted, like most real ones.
        for f in &mut o.fields {
            if f.name == "OwnerId" {
                f.required = true;
            }
        }
        o
    })
    .collect()
}

/// An Opportunity stage and what it implies.
pub struct Stage {
    pub name: &'static str,
    pub probability: f64,
    pub closed: bool,
    pub won: bool,
    pub forecast: &'static str,
}

/// The standard sales process's stages.
/// Account and Lead industries, as a new org has them.
const INDUSTRIES: &[&str] = &[
    "Agriculture",
    "Apparel",
    "Banking",
    "Biotechnology",
    "Chemicals",
    "Communications",
    "Construction",
    "Consulting",
    "Education",
    "Electronics",
    "Energy",
    "Engineering",
    "Entertainment",
    "Environmental",
    "Finance",
    "Food & Beverage",
    "Government",
    "Healthcare",
    "Hospitality",
    "Insurance",
    "Machinery",
    "Manufacturing",
    "Media",
    "Not For Profit",
    "Recreation",
    "Retail",
    "Shipping",
    "Technology",
    "Telecommunications",
    "Transportation",
    "Utilities",
    "Other",
];

pub const OPPORTUNITY_STAGES: &[Stage] = &[
    Stage { name: "Prospecting", probability: 10.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Qualification", probability: 10.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Needs Analysis", probability: 20.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Value Proposition", probability: 50.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Id. Decision Makers", probability: 60.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Perception Analysis", probability: 70.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Proposal/Price Quote", probability: 75.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Negotiation/Review", probability: 90.0, closed: false, won: false, forecast: "Pipeline" },
    Stage { name: "Closed Won", probability: 100.0, closed: true, won: true, forecast: "Closed" },
    Stage { name: "Closed Lost", probability: 0.0, closed: true, won: false, forecast: "Omitted" },
];

/// The Tooling API's objects (`/tooling/query`). Their rows are generated
/// from the catalog when they're queried (see `tooling`).
pub fn tooling_objects() -> Vec<SObject> {
    use FieldType::*;
    let s = |n: &str| Field::new(n, String);
    let apex = |name: &str, prefix: &str, extra: Vec<Field>| {
        let mut fields = vec![
            s("Name"),
            s("NamespacePrefix"),
            Field::new("ApiVersion", Double),
            pick_of("Status", &["Active", "Inactive", "Deleted"]),
            Field::new("IsValid", Boolean),
        ];
        fields.extend(extra);
        SObject::new(name, name, &format!("{name}es"), prefix, fields, false)
    };
    vec![
        apex("ApexClass", "01p", vec![Field::new("Body", Textarea), Field::new("LengthWithoutComments", Int)]),
        apex("ApexTrigger", "01q", vec![Field::new("Body", Textarea), s("TableEnumOrId")]),
        apex("ApexPage", "066", vec![Field::new("Markup", Textarea), s("MasterLabel")]),
        apex("ApexComponent", "099", vec![Field::new("Markup", Textarea), s("MasterLabel")]),
        SObject::new(
            "CustomObject",
            "Custom Object",
            "Custom Objects",
            "01I",
            vec![s("DeveloperName"), s("NamespacePrefix"), s("Description"), s("ManageableState")],
            false,
        ),
        SObject::new(
            "CustomField",
            "Custom Field",
            "Custom Fields",
            "00N",
            vec![s("DeveloperName"), s("NamespacePrefix"), s("TableEnumOrId"), s("Description")],
            false,
        ),
        SObject::new(
            "EntityDefinition",
            "Entity Definition",
            "Entity Definitions",
            "0Ed",
            vec![
                s("DurableId"),
                s("QualifiedApiName"),
                s("DeveloperName"),
                s("NamespacePrefix"),
                s("Label"),
                s("PluralLabel"),
                s("KeyPrefix"),
                Field::new("IsCustomizable", Boolean),
                Field::new("IsQueryable", Boolean),
                Field::new("IsCustomSetting", Boolean),
            ],
            false,
        ),
        SObject::new(
            "FieldDefinition",
            "Field Definition",
            "Field Definitions",
            "0Fd",
            vec![
                s("DurableId"),
                s("EntityDefinitionId"),
                s("QualifiedApiName"),
                s("DeveloperName"),
                s("NamespacePrefix"),
                s("Label"),
                s("DataType"),
                Field::new("Length", Int),
                Field::new("Precision", Int),
                Field::new("Scale", Int),
                Field::new("IsNillable", Boolean),
                Field::new("IsCalculated", Boolean),
            ],
            false,
        ),
        SObject::new(
            "ValidationRule",
            "Validation Rule",
            "Validation Rules",
            "03d",
            vec![
                s("ValidationName"),
                s("EntityDefinitionId"),
                Field::new("Active", Boolean),
                s("ErrorMessage"),
                s("ErrorDisplayField"),
                s("Description"),
                s("NamespacePrefix"),
            ],
            false,
        ),
    ]
}

fn pick_of(name: &str, values: &[&str]) -> Field {
    Field::new(name, FieldType::Picklist).picklist(values)
}

/// The fields of `Name`, what a polymorphic lookup leads to.
fn name_object() -> SObject {
    use FieldType::*;
    let s = |n: &str| Field::new(n, String).system();
    SObject::new(
        "Name",
        "Name",
        "Names",
        "000",
        vec![
            s("Name"),
            s("Type"),
            s("FirstName"),
            s("LastName"),
            s("Alias"),
            Field::new("Email", Email).system(),
            Field::new("Phone", Phone).system(),
            s("Title"),
            Field::new("Username", Email).system(),
            Field::new("IsActive", Boolean).system(),
        ],
        false,
    )
}

/// One layer of custom definitions: the `[[objects]]` and `[[fields]]`
/// tables in `localforce.toml`, what's read from SFDX metadata, or what's
/// been created in the web UI.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CustomSchema {
    #[serde(default)]
    pub objects: Vec<CustomObjectDef>,
    #[serde(default)]
    pub fields: Vec<CustomFieldOnStandard>,
    #[serde(default)]
    pub validation_rules: Vec<ValidationRuleDef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomObjectDef {
    pub name: String,
    pub label: Option<String>,
    pub label_plural: Option<String>,
    pub description: Option<String>,
    /// The label of the Name field, e.g. "Invoice Number".
    pub name_field_label: Option<String>,
    /// Makes Name an auto-number with this display format, e.g. `INV-{0000}`.
    pub name_field_format: Option<String>,
    /// Fixed for objects created in the UI, so record ids stay valid.
    pub key_prefix: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomFieldOnStandard {
    pub object: String,
    #[serde(flatten)]
    pub field: FieldDef,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: FieldType,
    pub label: Option<String>,
    pub description: Option<String>,
    pub length: Option<u32>,
    pub precision: Option<u32>,
    pub scale: Option<u32>,
    #[serde(default)]
    pub required: bool,
    pub reference_to: Option<String>,
    pub relationship_name: Option<String>,
    pub child_relationship_name: Option<String>,
    /// Master-detail: required, and deleting the parent deletes this record.
    #[serde(default)]
    pub master_detail: bool,
    /// A lookup that stops its parent from being deleted while it's set.
    #[serde(default)]
    pub restrict_delete: bool,
    /// Master-detail only: whether a detail can be moved to another master.
    #[serde(default)]
    pub reparentable: bool,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub restricted: bool,
    pub default: Option<serde_json::Value>,
    /// A default value that's a formula, such as `TODAY() + 30` or `$User.Email`.
    pub default_formula: Option<String>,
    #[serde(default)]
    pub external_id: bool,
    #[serde(default)]
    pub unique: bool,
    /// Makes this an auto-number field with this display format.
    pub auto_number_format: Option<String>,
    /// Makes this a read-only formula field; `type` is the result type.
    pub formula: Option<String>,
    /// For a numeric formula: whether blank fields count as zero (the default) or make the result blank.
    pub blank_as_zero: Option<bool>,
    /// Makes this a roll-up summary of the detail records of a master-detail relationship.
    pub rollup: Option<Rollup>,
}

impl FieldDef {
    pub fn to_field(&self) -> anyhow::Result<Field> {
        validate_custom_name(&self.name, "field")?;
        let mut f = Field::new(&self.name, self.ty);
        anyhow::ensure!(!matches!(self.ty, FieldType::Id), "{}: custom fields can't have type id", self.name);
        if let Some(l) = &self.label {
            f.label = l.clone();
        }
        f.description = self.description.clone();
        if self.length.is_some() {
            f.length = self.length;
        }
        f.precision = self.precision;
        f.scale = self.scale;
        f.required = self.required || self.master_detail;
        f.picklist = self.values.clone();
        f.restricted = self.restricted;
        f.external_id = self.external_id;
        f.unique = self.unique;
        f.default = self.default.clone();
        f.default_formula = self.default_formula.clone().filter(|s| !s.trim().is_empty());
        anyhow::ensure!(
            f.default.is_none() || f.default_formula.is_none(),
            "{}: give a default or a default_formula, not both",
            self.name
        );
        if matches!(self.ty, FieldType::Picklist | FieldType::Multipicklist)
            && self.values.is_empty()
            && self.restricted
        {
            anyhow::bail!("{}: a restricted picklist needs values", self.name);
        }
        if self.ty == FieldType::Reference {
            let to = self.reference_to.clone().ok_or_else(|| anyhow::anyhow!("{} needs reference_to", self.name))?;
            f.reference_to = Some(to);
            f.relationship_name = Some(
                self.relationship_name.clone().unwrap_or_else(|| format!("{}__r", self.name.trim_end_matches("__c"))),
            );
            f.child_relationship_name = self.child_relationship_name.clone();
            f.master_detail = self.master_detail;
            f.cascade_delete = self.master_detail;
            // A master-detail field can only be changed if it's reparentable.
            f.updateable = !self.master_detail || self.reparentable;
            f.restrict_delete = self.restrict_delete && !self.master_detail;
        }
        if let Some(fmt) = &self.auto_number_format {
            f.ty = FieldType::String;
            f.length = Some(30);
            f.required = false;
            f.derived = Derived::AutoNumber(fmt.clone());
            f.createable = false;
            f.updateable = false;
        }
        if let Some(formula) = &self.formula {
            f.required = false;
            f.derived = Derived::Formula(formula.clone());
            f.createable = false;
            f.updateable = false;
            f.blank_as_zero = self.blank_as_zero.unwrap_or(true);
        }
        if let Some(r) = &self.rollup {
            anyhow::ensure!(
                matches!(r.op.as_str(), "count" | "sum" | "min" | "max"),
                "{}: a roll-up's op is count, sum, min or max",
                self.name
            );
            anyhow::ensure!(r.op == "count" || r.field.is_some(), "{}: a {} roll-up needs a field", self.name, r.op);
            f.required = false;
            f.derived = Derived::Rollup(r.clone());
            f.createable = false;
            f.updateable = false;
        }
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_number_formats() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-03-04T00:00:00Z").unwrap().to_utc();
        assert_eq!(format_auto_number("INV-{0000}", 42, now), "INV-0042");
        assert_eq!(format_auto_number("{YYYY}{MM}-{0}", 7, now), "202603-7");
    }

    #[test]
    fn derived_prefixes_are_stable_and_distinct() {
        let a = derive_prefix("Invoice__c", &[]);
        assert_eq!(a, derive_prefix("invoice__c", &[]));
        assert!(a.starts_with('a') && a.len() == 3);
        assert_ne!(derive_prefix("Invoice__c", std::slice::from_ref(&a)), a);
    }

    #[test]
    fn api_names() {
        assert!(validate_custom_name("Invoice_Line__c", "object").is_ok());
        assert!(validate_custom_name("Invoice", "object").is_err());
        assert!(validate_custom_name("Bad__Name__c", "object").is_err());
        assert!(validate_custom_name("1Bad__c", "object").is_err());
    }
}
