//! Describe results, generated from the catalog.

use serde_json::{Value, json};

use crate::schema::{Catalog, Derived, Field, FieldType, SObject};

fn urls(v: &str, o: &SObject) -> Value {
    let base = format!("/services/data/v{v}/sobjects/{}", o.name);
    json!({
        "compactLayouts": format!("{base}/describe/compactLayouts"),
        "rowTemplate": format!("{base}/{{ID}}"),
        "approvalLayouts": format!("{base}/describe/approvalLayouts"),
        "listviews": format!("{base}/listviews"),
        "layouts": format!("{base}/describe/layouts"),
        "defaultValues": format!("{base}/defaultValues?recordTypeId&fields"),
        "describe": format!("{base}/describe"),
        "sobject": base,
    })
}

pub fn global_entry(v: &str, o: &SObject) -> Value {
    let user = o.name == "User";
    json!({
        "activateable": false,
        "createable": true,
        "custom": o.custom,
        "customSetting": false,
        "deepCloneable": false,
        "deletable": !user,
        "deprecatedAndHidden": false,
        "feedEnabled": false,
        "hasSubtypes": false,
        "isInterface": false,
        "isSubtype": false,
        "keyPrefix": o.key_prefix,
        "label": o.label,
        "labelPlural": o.label_plural,
        "layoutable": true,
        "mergeable": matches!(o.name.as_str(), "Account" | "Contact" | "Lead"),
        "mruEnabled": true,
        "name": o.name,
        "queryable": true,
        "replicateable": true,
        "retrieveable": true,
        "searchable": true,
        "triggerable": true,
        "undeletable": !user,
        "updateable": true,
        "urls": urls(v, o),
    })
}

pub fn global(v: &str, cat: &Catalog) -> Value {
    json!({
        "encoding": "UTF-8",
        "maxBatchSize": 200,
        "sobjects": cat.objects.iter().map(|o| global_entry(v, o)).collect::<Vec<_>>(),
    })
}

pub(crate) fn field(o: &SObject, f: &Field) -> Value {
    let is_name = f.name == "Name" || (o.name == "Case" && f.name == "CaseNumber");
    let numeric = matches!(f.ty, FieldType::Double | FieldType::Currency | FieldType::Percent);
    let text = f.ty.is_text() || matches!(f.ty, FieldType::Id | FieldType::Reference);
    json!({
        "aggregatable": f.ty != FieldType::Textarea,
        "autoNumber": matches!(f.derived, Derived::AutoNumber(_)),
        "byteLength": if text { f.length.unwrap_or(0) * 3 } else { 0 },
        "calculated": matches!(f.derived, Derived::FullName | Derived::Formula(_) | Derived::Rollup(_)),
        "calculatedFormula": match &f.derived { Derived::Formula(src) => Some(src.as_str()), _ => None },
        "cascadeDelete": f.cascade_delete,
        "caseSensitive": matches!(f.ty, FieldType::Id | FieldType::Reference),
        "createable": f.createable,
        "custom": f.custom,
        "defaultValue": f.default,
        "defaultValueFormula": f.default_formula,
        "defaultedOnCreate": f.default.is_some() || f.default_formula.is_some() || f.derived != Derived::None || f.name == "OwnerId" || f.ty == FieldType::Boolean,
        "dependentPicklist": false,
        "deprecatedAndHidden": false,
        "digits": if f.ty == FieldType::Int { 9 } else { 0 },
        "externalId": f.external_id,
        "filterable": f.filterable(),
        "groupable": f.groupable(),
        "idLookup": f.name == "Id" || f.external_id,
        "label": f.label,
        "length": if text { f.length.unwrap_or(0) } else { 0 },
        "name": f.name,
        "nameField": is_name,
        "nillable": f.nillable(),
        "picklistValues": f.picklist.iter().map(|v| json!({
            "active": true,
            "defaultValue": f.default.as_ref().and_then(|d| d.as_str()) == Some(v.as_str()),
            "label": v,
            "validFor": null,
            "value": v,
        })).collect::<Vec<_>>(),
        "precision": if numeric { f.precision.unwrap_or(18) } else { 0 },
        "referenceTo": f.targets().collect::<Vec<_>>(),
        "relationshipName": f.relationship_name,
        "restrictedPicklist": f.restricted,
        "scale": if numeric { f.scale.unwrap_or(2) } else { 0 },
        "soapType": f.ty.soap_type(),
        "sortable": f.sortable(),
        "type": f.ty.describe_name(),
        "unique": f.unique,
        "updateable": f.updateable,
    })
}

pub fn sobject(v: &str, cat: &Catalog, o: &SObject) -> Value {
    let mut d = global_entry(v, o);
    d["fields"] = o.fields.iter().map(|f| field(o, f)).collect();
    d["childRelationships"] = cat
        .children_of(&o.name)
        .iter()
        .map(|c| {
            json!({
                "cascadeDelete": c.cascade_delete,
                "childSObject": c.child_object,
                "deprecatedAndHidden": false,
                "field": c.field,
                "junctionIdListNames": [],
                "junctionReferenceTo": [],
                "relationshipName": c.name,
                "restrictedDelete": c.restrict_delete,
            })
        })
        .collect();
    d["recordTypeInfos"] = json!([]);
    d["supportedScopes"] = json!([{ "label": "All", "name": "everything" }, { "label": "Mine", "name": "mine" }]);
    d["actionOverrides"] = json!([]);
    d["namedLayoutInfos"] = crate::layouts::named_layouts(o)
        .iter()
        .map(|n| json!({ "name": n, "urls": { "layout": format!("/services/data/v{v}/sobjects/{}/describe/namedLayouts/{n}", o.name) } }))
        .collect();
    d["networkScopeFieldName"] = Value::Null;
    d["sobjectDescribeOption"] = json!("FULL");
    d
}
