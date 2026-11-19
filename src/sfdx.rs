//! Reads custom objects and fields from Salesforce metadata, so localforce
//! can emulate a team's own org straight from their repository.
//!
//! Both layouts are understood:
//!
//! - SFDX source format: `objects/Invoice__c/Invoice__c.object-meta.xml` with
//!   one `fields/Amount__c.field-meta.xml` per field;
//! - Metadata API format: `objects/Invoice__c.object` with inline `<fields>`.
//!
//! Point `--metadata` at a project (`force-app`, or the folder holding
//! `sfdx-project.json`) and every `objects` folder under it is read. Picklists
//! that use a global value set are resolved from `globalValueSets`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use roxmltree::{Document, Node};
use serde_json::Value;

use crate::schema::{
    CustomFieldOnStandard, CustomObjectDef, CustomSchema, FieldDef, FieldType, Rollup, RollupFilter, ValidationRule,
    ValidationRuleDef,
};

/// What was loaded, plus anything skipped and why (logged at startup).
pub struct Loaded {
    pub schema: CustomSchema,
    pub skipped: Vec<String>,
}

pub fn load(root: &Path) -> anyhow::Result<Loaded> {
    anyhow::ensure!(root.exists(), "metadata folder {} doesn't exist", root.display());
    let mut object_dirs = vec![];
    let mut value_set_dirs = vec![];
    find_dirs(root, &mut object_dirs, &mut value_set_dirs)?;

    let mut value_sets = HashMap::new();
    for dir in &value_set_dirs {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(set) =
                name.strip_suffix(".globalValueSet-meta.xml").or_else(|| name.strip_suffix(".globalValueSet"))
            else {
                continue;
            };
            let text = std::fs::read_to_string(&path)?;
            let doc = Document::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
            let values = doc
                .root_element()
                .children()
                .filter(|n| n.has_tag_name("customValue"))
                .filter_map(|n| child_text(n, "fullName"))
                .collect::<Vec<_>>();
            value_sets.insert(set.to_string(), values);
        }
    }

    let mut out = Loaded { schema: CustomSchema::default(), skipped: vec![] };
    for dir in &object_dirs {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                load_source_object(&path, &name, &value_sets, &mut out)?;
            } else if let Some(object) = name.strip_suffix(".object") {
                let text = std::fs::read_to_string(&path)?;
                let doc = Document::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
                let fields: Vec<Node> = doc.root_element().children().filter(|n| n.has_tag_name("fields")).collect();
                add_object(object, Some(doc.root_element()), &fields, &path, &value_sets, &mut out)?;
                for rule in doc.root_element().children().filter(|n| n.has_tag_name("validationRules")) {
                    add_rule(object, rule, &mut out);
                }
            }
        }
    }
    Ok(out)
}

fn find_dirs(dir: &Path, objects: &mut Vec<PathBuf>, value_sets: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        match path.file_name().and_then(|n| n.to_str()) {
            Some("node_modules" | ".git" | ".sfdx" | ".sf") => {}
            Some("objects") => objects.push(path),
            Some("globalValueSets") => value_sets.push(path),
            _ => find_dirs(&path, objects, value_sets)?,
        }
    }
    Ok(())
}

/// `objects/<Object>/` in source format.
fn load_source_object(
    dir: &Path,
    object: &str,
    value_sets: &HashMap<String, Vec<String>>,
    out: &mut Loaded,
) -> anyhow::Result<()> {
    let object_file = dir.join(format!("{object}.object-meta.xml"));
    let object_text = std::fs::read_to_string(&object_file).ok();
    let object_doc = object_text
        .as_deref()
        .map(Document::parse)
        .transpose()
        .with_context(|| format!("parsing {}", object_file.display()))?;

    let mut field_texts = vec![];
    let fields_dir = dir.join("fields");
    if fields_dir.is_dir() {
        let mut paths: Vec<PathBuf> =
            std::fs::read_dir(&fields_dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
        paths.sort();
        for p in paths {
            if p.to_string_lossy().ends_with(".field-meta.xml") {
                field_texts.push((std::fs::read_to_string(&p)?, p));
            }
        }
    }
    let field_docs = field_texts
        .iter()
        .map(|(t, p)| Document::parse(t).with_context(|| format!("parsing {}", p.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let fields: Vec<Node> = field_docs.iter().map(|d| d.root_element()).collect();
    add_object(object, object_doc.as_ref().map(|d| d.root_element()), &fields, dir, value_sets, out)?;

    let rules_dir = dir.join("validationRules");
    if rules_dir.is_dir() {
        let mut paths: Vec<PathBuf> =
            std::fs::read_dir(&rules_dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
        paths.sort();
        for p in paths.iter().filter(|p| p.to_string_lossy().ends_with(".validationRule-meta.xml")) {
            let text = std::fs::read_to_string(p)?;
            let doc = Document::parse(&text).with_context(|| format!("parsing {}", p.display()))?;
            add_rule(object, doc.root_element(), out);
        }
    }
    Ok(())
}

/// A `<ValidationRule>` (source format) or `<validationRules>` (in a `.object` file).
fn add_rule(object: &str, n: Node, out: &mut Loaded) {
    let (Some(name), Some(condition)) = (child_text(n, "fullName"), child_text(n, "errorConditionFormula")) else {
        out.skipped.push(format!("{object}: a validation rule without a fullName or errorConditionFormula"));
        return;
    };
    out.schema.validation_rules.push(ValidationRuleDef {
        object: object.to_string(),
        rule: ValidationRule {
            name,
            condition,
            message: child_text(n, "errorMessage").unwrap_or_default(),
            field: child_text(n, "errorDisplayField"),
            active: child_text(n, "active").as_deref() != Some("false"),
            description: child_text(n, "description"),
        },
    });
}

fn add_object(
    object: &str,
    object_el: Option<Node>,
    fields: &[Node],
    path: &Path,
    value_sets: &HashMap<String, Vec<String>>,
    out: &mut Loaded,
) -> anyhow::Result<()> {
    let custom = object.ends_with("__c");
    if !custom && object.contains("__") {
        // Custom metadata types, platform events, big objects, external objects…
        out.skipped.push(format!("{object}: only custom objects (__c) are supported"));
        return Ok(());
    }
    let mut defs = vec![];
    for f in fields {
        let Some(name) = child_text(*f, "fullName") else { continue };
        if !name.ends_with("__c") {
            // Standard fields appear here only to customise them (e.g. picklist values).
            continue;
        }
        match field_def(*f, &name, value_sets) {
            Ok(d) => defs.push(d),
            Err(reason) => out.skipped.push(format!("{object}.{name}: {reason}")),
        }
    }
    if custom {
        let el = object_el.ok_or_else(|| anyhow::anyhow!("{}: missing {object}.object-meta.xml", path.display()))?;
        let name_field = el.children().find(|n| n.has_tag_name("nameField"));
        let auto = name_field.and_then(|n| child_text(n, "type")).as_deref() == Some("AutoNumber");
        out.schema.objects.push(CustomObjectDef {
            name: object.to_string(),
            label: child_text(el, "label"),
            label_plural: child_text(el, "pluralLabel"),
            description: child_text(el, "description"),
            name_field_label: name_field.and_then(|n| child_text(n, "label")),
            name_field_format: if auto { name_field.and_then(|n| child_text(n, "displayFormat")) } else { None },
            key_prefix: None,
            fields: defs,
        });
    } else {
        out.schema
            .fields
            .extend(defs.into_iter().map(|field| CustomFieldOnStandard { object: object.to_string(), field }));
    }
    Ok(())
}

fn child_text(n: Node, tag: &str) -> Option<String> {
    n.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text()).map(|t| t.trim().to_string())
}

fn child_bool(n: Node, tag: &str) -> bool {
    child_text(n, tag).as_deref() == Some("true")
}

fn child_u32(n: Node, tag: &str) -> Option<u32> {
    child_text(n, tag).and_then(|t| t.parse().ok())
}

/// A `<defaultValue>` is a formula: a literal (`"text"`, `true`, `12.5`) is
/// kept as a plain default, anything else as a default-value formula.
fn default_value(n: Node) -> (Option<Value>, Option<String>) {
    let Some(raw) = child_text(n, "defaultValue") else { return (None, None) };
    let raw = raw.trim().to_string();
    if let Some(s) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
        && !s.contains('"')
    {
        return (Some(Value::String(s.to_string())), None);
    }
    match raw.to_ascii_lowercase().as_str() {
        "true" => return (Some(Value::Bool(true)), None),
        "false" => return (Some(Value::Bool(false)), None),
        _ => {}
    }
    match raw.parse::<f64>() {
        Ok(f) => (Some(serde_json::json!(f)), None),
        Err(_) => (None, Some(raw)),
    }
}

fn field_def(n: Node, name: &str, value_sets: &HashMap<String, Vec<String>>) -> Result<FieldDef, String> {
    let ty_name = child_text(n, "type");
    let formula = child_text(n, "formula");
    let summary = ty_name.as_deref() == Some("Summary");
    let ty = match (ty_name.as_deref(), formula.is_some()) {
        // A formula's <type> is its result type.
        (Some("Text" | "EncryptedText" | "Time"), _) => FieldType::String,
        (Some("TextArea" | "LongTextArea" | "Html"), _) => FieldType::Textarea,
        (Some("Email"), _) => FieldType::Email,
        (Some("Phone"), _) => FieldType::Phone,
        (Some("Url"), _) => FieldType::Url,
        (Some("Picklist"), _) => FieldType::Picklist,
        (Some("MultiselectPicklist"), _) => FieldType::Multipicklist,
        (Some("Checkbox"), _) => FieldType::Boolean,
        (Some("Number" | "Summary"), _) => FieldType::Double,
        (Some("Currency"), _) => FieldType::Currency,
        (Some("Percent"), _) => FieldType::Percent,
        (Some("Date"), _) => FieldType::Date,
        (Some("DateTime"), _) => FieldType::Datetime,
        (Some("Lookup" | "MasterDetail" | "Hierarchy"), _) => FieldType::Reference,
        (Some("AutoNumber"), _) => FieldType::String,
        (Some(other), _) => return Err(format!("field type {other} isn't supported yet")),
        (None, _) => return Err("no <type>".into()),
    };
    let master_detail = ty_name.as_deref() == Some("MasterDetail");
    let mut values = vec![];
    let mut restricted = false;
    let (mut default, default_formula) = default_value(n);
    if let Some(vs) = n.children().find(|c| c.has_tag_name("valueSet")) {
        restricted = child_bool(vs, "restricted");
        if let Some(set) = child_text(vs, "valueSetName") {
            values = value_sets.get(&set).cloned().ok_or_else(|| format!("global value set {set} wasn't found"))?;
            // Global value sets are always restricted.
            restricted = true;
        } else if let Some(def) = vs.children().find(|c| c.has_tag_name("valueSetDefinition")) {
            for v in def.children().filter(|c| c.has_tag_name("value")) {
                if let Some(full) = child_text(v, "fullName") {
                    if child_bool(v, "default") {
                        default = Some(Value::String(full.clone()));
                    }
                    values.push(full);
                }
            }
        }
    }
    let reference_to = match ty_name.as_deref() {
        Some("Hierarchy") => Some("User".to_string()),
        _ => child_text(n, "referenceTo"),
    };
    Ok(FieldDef {
        name: name.to_string(),
        ty,
        label: child_text(n, "label"),
        description: child_text(n, "description"),
        length: child_u32(n, "length"),
        precision: child_u32(n, "precision"),
        scale: child_u32(n, "scale"),
        required: child_bool(n, "required"),
        reference_to,
        relationship_name: None,
        // In metadata, <relationshipName> is the child relationship, without __r.
        child_relationship_name: child_text(n, "relationshipName").map(|r| format!("{r}__r")),
        master_detail,
        restrict_delete: child_text(n, "deleteConstraint").as_deref() == Some("Restrict"),
        reparentable: child_bool(n, "reparentableMasterDetail"),
        values,
        restricted,
        default,
        default_formula,
        external_id: child_bool(n, "externalId"),
        unique: child_bool(n, "unique"),
        auto_number_format: if ty_name.as_deref() == Some("AutoNumber") {
            child_text(n, "displayFormat")
        } else {
            None
        },
        blank_as_zero: formula
            .as_ref()
            .map(|_| child_text(n, "formulaTreatBlanksAs").as_deref() != Some("BlankAsBlank")),
        formula,
        rollup: if summary { Some(rollup(n)?) } else { None },
    })
}

/// A roll-up summary field's `<summaryOperation>`, `<summaryForeignKey>`, … .
fn rollup(n: Node) -> Result<Rollup, String> {
    let op = child_text(n, "summaryOperation").ok_or("a roll-up summary without a summaryOperation")?;
    let fk = child_text(n, "summaryForeignKey").ok_or("a roll-up summary without a summaryForeignKey")?;
    let (object, via) = fk.split_once('.').ok_or("summaryForeignKey isn't Object.Field")?;
    let after_dot = |s: String| s.split_once('.').map(|(_, f)| f.to_string()).unwrap_or(s);
    let filters = n
        .children()
        .filter(|c| c.has_tag_name("summaryFilterItems"))
        .map(|c| {
            Ok(RollupFilter {
                field: after_dot(child_text(c, "field").ok_or("a summary filter without a field")?),
                operation: child_text(c, "operation").ok_or("a summary filter without an operation")?,
                value: child_text(c, "value").unwrap_or_default(),
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Rollup {
        op: op.to_ascii_lowercase(),
        object: object.to_string(),
        via: via.to_string(),
        field: child_text(n, "summarizedField").map(after_dot),
        filters,
    })
}

/// Render an object's custom definitions as SFDX source files, so what's
/// designed in the UI can be committed to the project. Paths are relative to
/// `force-app/main/default`.
pub fn export(obj: &crate::schema::SObject) -> Vec<(String, String)> {
    use crate::schema::Derived;
    const HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
    const NS: &str = "xmlns=\"http://soap.sforce.com/2006/04/metadata\"";
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let tag = |name: &str, value: &str| format!("    <{name}>{}</{name}>\n", esc(value));
    let mut files = vec![];
    if obj.custom {
        let name_field = obj.field("Name");
        let child_of_master = obj.fields.iter().any(|f| f.master_detail);
        let mut x = format!("{HEAD}<CustomObject {NS}>\n");
        x += &tag("deploymentStatus", "Deployed");
        if let Some(d) = &obj.description {
            x += &tag("description", d);
        }
        x += &tag("label", &obj.label);
        x += "    <nameField>\n";
        if let Some(f) = name_field {
            if let Derived::AutoNumber(fmt) = &f.derived {
                x += &format!("    {}", tag("displayFormat", fmt));
                x += &format!("    {}", tag("label", &f.label));
                x += &format!("    {}", tag("type", "AutoNumber"));
            } else {
                x += &format!("    {}", tag("label", &f.label));
                x += &format!("    {}", tag("type", "Text"));
            }
        }
        x += "    </nameField>\n";
        x += &tag("pluralLabel", &obj.label_plural);
        x += &tag("sharingModel", if child_of_master { "ControlledByParent" } else { "ReadWrite" });
        x += "</CustomObject>\n";
        files.push((format!("objects/{0}/{0}.object-meta.xml", obj.name), x));
    }
    for f in obj.fields.iter().filter(|f| f.custom) {
        let mut x = format!("{HEAD}<CustomField {NS}>\n");
        x += &tag("fullName", &f.name);
        if let Some(d) = &f.description {
            x += &tag("description", d);
        }
        let formula = match &f.derived {
            Derived::Formula(src) => Some(src.clone()),
            _ => None,
        };
        let rollup = match &f.derived {
            Derived::Rollup(r) => Some(r.clone()),
            _ => None,
        };
        let auto = match &f.derived {
            Derived::AutoNumber(fmt) => Some(fmt.clone()),
            _ => None,
        };
        let sf_type = if auto.is_some() {
            "AutoNumber"
        } else if rollup.is_some() {
            "Summary"
        } else {
            match f.ty {
                FieldType::String | FieldType::Id => "Text",
                FieldType::Textarea if f.length.unwrap_or(255) > 255 => "LongTextArea",
                FieldType::Textarea => "TextArea",
                FieldType::Email => "Email",
                FieldType::Phone => "Phone",
                FieldType::Url => "Url",
                FieldType::Picklist => "Picklist",
                FieldType::Multipicklist => "MultiselectPicklist",
                FieldType::Boolean => "Checkbox",
                FieldType::Int | FieldType::Double => "Number",
                FieldType::Currency => "Currency",
                FieldType::Percent => "Percent",
                FieldType::Date => "Date",
                FieldType::Datetime => "DateTime",
                FieldType::Reference if f.master_detail => "MasterDetail",
                FieldType::Reference => "Lookup",
                FieldType::Address => "Address",
            }
        };
        if let Some(fmt) = &auto {
            x += &tag("displayFormat", fmt);
        }
        if f.ty == FieldType::Reference && !f.master_detail {
            x += &tag("deleteConstraint", if f.restrict_delete { "Restrict" } else { "SetNull" });
        }
        if let Some(src) = &formula {
            x += &tag("formula", src);
            if matches!(f.ty, FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent) {
                x += &tag("formulaTreatBlanksAs", if f.blank_as_zero { "BlankAsZero" } else { "BlankAsBlank" });
            }
        }
        if let Some(src) = &f.default_formula {
            x += &tag("defaultValue", src);
        } else if f.ty == FieldType::Boolean && formula.is_none() {
            let on = f.default.as_ref().and_then(|d| d.as_bool()).unwrap_or(false);
            x += &tag("defaultValue", if on { "true" } else { "false" });
        } else if let Some(d) = &f.default
            && !matches!(f.ty, FieldType::Picklist | FieldType::Multipicklist)
        {
            let v = match d {
                serde_json::Value::String(s) => format!("\"{s}\""),
                other => other.to_string(),
            };
            x += &tag("defaultValue", &v);
        }
        if f.external_id {
            x += &tag("externalId", "true");
        }
        x += &tag("label", &f.label);
        match sf_type {
            "Text" | "TextArea" | "LongTextArea" if auto.is_none() && formula.is_none() => {
                if sf_type != "TextArea" {
                    x += &tag("length", &f.length.unwrap_or(255).to_string());
                }
            }
            "Number" | "Currency" | "Percent" => {
                x += &tag("precision", &f.precision.unwrap_or(18).to_string());
                x += &tag("scale", &f.scale.unwrap_or(if f.ty == FieldType::Int { 0 } else { 2 }).to_string());
            }
            _ => {}
        }
        if let Some(to) = &f.reference_to {
            x += &tag("referenceTo", to);
            let child = f
                .child_relationship_name
                .clone()
                .unwrap_or_else(|| format!("{}s__r", obj.name.trim_end_matches("__c")));
            x += &tag("relationshipLabel", &obj.label_plural);
            x += &tag("relationshipName", child.trim_end_matches("__r"));
            if f.master_detail {
                x += &tag("relationshipOrder", "0");
                x += &tag("reparentableMasterDetail", if f.updateable { "true" } else { "false" });
                x += &tag("writeRequiresMasterRead", "false");
            }
        }
        if f.required && !f.master_detail && formula.is_none() && auto.is_none() {
            x += &tag("required", "true");
        }
        if let Some(r) = &rollup {
            x += &tag("summaryForeignKey", &format!("{}.{}", r.object, r.via));
            for flt in &r.filters {
                x += &format!(
                    "    <summaryFilterItems>\n        <field>{}.{}</field>\n        <operation>{}</operation>\n        <value>{}</value>\n    </summaryFilterItems>\n",
                    esc(&r.object),
                    esc(&flt.field),
                    esc(&flt.operation),
                    esc(&flt.value)
                );
            }
            x += &tag("summaryOperation", &r.op);
            if let Some(field) = &r.field {
                x += &tag("summarizedField", &format!("{}.{field}", r.object));
            }
        }
        x += &tag("type", sf_type);
        if f.unique {
            x += &tag("unique", "true");
        }
        if matches!(f.ty, FieldType::Picklist | FieldType::Multipicklist) {
            x += "    <valueSet>\n";
            if f.restricted {
                x += "        <restricted>true</restricted>\n";
            }
            x += "        <valueSetDefinition>\n            <sorted>false</sorted>\n";
            for v in &f.picklist {
                let is_default = f.default.as_ref().and_then(|d| d.as_str()) == Some(v.as_str());
                x += &format!(
                    "            <value>\n                <fullName>{0}</fullName>\n                <default>{is_default}</default>\n                <label>{0}</label>\n            </value>\n",
                    esc(v)
                );
            }
            x += "        </valueSetDefinition>\n    </valueSet>\n";
            if f.ty == FieldType::Multipicklist {
                x += &tag("visibleLines", "4");
            }
        } else if sf_type == "LongTextArea" {
            x += &tag("visibleLines", "3");
        }
        x += "</CustomField>\n";
        files.push((format!("objects/{}/fields/{}.field-meta.xml", obj.name, f.name), x));
    }
    files
}
