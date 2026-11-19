//! Page layouts (`describe/layouts`, `describe/namedLayouts`, `describe/compactLayouts`,
//! `describe/approvalLayouts`), generated from each object's fields the way a new
//! org's default layout looks: an information section with the editable fields in
//! two columns, a system information section, the standard buttons and a related
//! list per child relationship.

use serde_json::{Value, json};

use crate::describe;
use crate::schema::{Catalog, SObject};

/// The master record type every object has.
pub const MASTER_RECORD_TYPE: &str = "012000000000000AAA";

const SYSTEM: &[&str] = &["Id", "IsDeleted", "SystemModstamp", "CreatedDate", "LastModifiedDate"];
const AUDIT: &[&str] = &["CreatedById", "LastModifiedById"];

/// A stable id for something named, e.g. an object's layout: the same in every org.
pub fn stable_id(prefix: &str, name: &str) -> String {
    // FNV-1a, kept within 9 base-62 digits.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in name.to_ascii_lowercase().bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    crate::ids::make(prefix, h % 13_537_086_546_263_552)
}

fn button(name: &str) -> Value {
    json!({
        "behavior": null, "colors": null, "content": null, "contentSource": null, "custom": false,
        "encoding": null, "height": null, "icons": null, "label": name, "menubar": false, "name": name,
        "overridden": false, "resizeable": false, "scrollbars": false, "showsLocation": false,
        "showsStatus": false, "toolbar": false, "url": null, "width": null, "windowPosition": null,
    })
}

fn field_item(o: &SObject, name: &str, tab: usize, edit: bool) -> Value {
    let f = o.field(name).expect("layout fields come from the object");
    json!({
        "editableForNew": edit && f.createable,
        "editableForUpdate": edit && f.updateable,
        "label": f.label,
        "layoutComponents": [{
            "details": describe::field(o, f),
            "displayLines": if f.ty == crate::schema::FieldType::Textarea { 3 } else { 1 },
            "tabOrder": tab,
            "type": "Field",
            "value": f.name,
        }],
        "placeholder": false,
        "required": edit && f.required && f.createable,
    })
}

fn blank(tab: usize) -> Value {
    json!({
        "editableForNew": false, "editableForUpdate": false, "label": "",
        "layoutComponents": [{ "displayLines": 1, "tabOrder": tab, "type": "EmptySpace" }],
        "placeholder": true, "required": false,
    })
}

/// A two-column section, filled top to bottom: the first half of the fields on the left.
fn section(o: &SObject, layout_id: &str, n: usize, heading: &str, fields: &[&str], edit: bool) -> Value {
    let rows = fields.len().div_ceil(2);
    let item = |i: usize| match fields.get(i) {
        Some(f) => field_item(o, f, i + 1, edit),
        None => blank(i + 1),
    };
    let layout_rows: Vec<Value> =
        (0..rows).map(|r| json!({ "layoutItems": [item(r), item(r + rows)], "numItems": 2 })).collect();
    json!({
        "collapsed": false,
        "columns": 2,
        "heading": heading,
        "layoutRows": layout_rows,
        "layoutSectionId": stable_id("01B", &format!("{}.{n}", o.name)),
        "parentLayoutId": layout_id,
        "rows": rows,
        "tabOrder": "TopToBottom",
        "useCollapsibleSection": false,
        "useHeading": true,
    })
}

fn related_lists(cat: &Catalog, o: &SObject) -> Vec<Value> {
    cat.children_of(&o.name)
        .iter()
        .filter_map(|c| {
            let child = cat.object(&c.child_object)?;
            let name = child.name_field()?;
            let label = child.label_plural.clone();
            Some(json!({
                "accessLevelRequiredForCreate": null,
                "buttons": [button("New")],
                "columns": [{
                    "field": format!("{}.{}", child.name, name.name),
                    "fieldApiName": name.name,
                    "format": null,
                    "label": name.label,
                    "lookupId": "Id",
                    "name": name.name,
                    "sortable": true,
                }],
                "custom": child.custom,
                "field": c.field,
                "label": label,
                "limitRows": 5,
                "name": c.name,
                "sobject": child.name,
                "sort": [],
            }))
        })
        .collect()
}

/// The layout itself, as `describe/layouts/{recordTypeId}` returns it.
pub fn layout(cat: &Catalog, o: &SObject, named: Option<&str>) -> Value {
    let id = stable_id("00h", &format!("{}.{}", o.name, named.unwrap_or("")));
    let main: Vec<&str> = o
        .fields
        .iter()
        .map(|f| f.name.as_str())
        .filter(|n| !SYSTEM.contains(n) && !AUDIT.contains(n))
        .filter(|n| !(o.name == "User" && *n == "Name"))
        .collect();
    let audit: Vec<&str> = AUDIT.iter().copied().filter(|n| o.field(n).is_some()).collect();
    let heading = format!("{} Information", o.label);
    let sections = |edit: bool| {
        let mut s = vec![section(o, &id, 0, &heading, &main, edit)];
        if !audit.is_empty() && !edit {
            s.push(section(o, &id, 1, "System Information", &audit, false));
        }
        json!(s)
    };
    json!({
        "buttonLayoutSection": { "detailButtons": [button("Edit"), button("Delete"), button("Clone")] },
        "detailLayoutSections": sections(false),
        "editLayoutSections": sections(true),
        "feedView": null,
        "highlightsPanelLayoutSection": null,
        "id": id,
        "multirowEditLayoutSections": [],
        "offlineLinks": [],
        "quickActionList": { "quickActionListItems": [] },
        "relatedContent": null,
        "relatedLists": related_lists(cat, o),
        "saveOptions": [],
    })
}

fn record_type_urls(v: &str, o: &SObject) -> Value {
    json!({ "layout": format!("/services/data/v{v}/sobjects/{}/describe/layouts/{MASTER_RECORD_TYPE}", o.name) })
}

/// `describe/layouts`, or `describe/namedLayouts/{name}`.
pub fn layouts(v: &str, cat: &Catalog, o: &SObject, named: Option<&str>) -> Value {
    let l = layout(cat, o, named);
    json!({
        "layouts": [l],
        "recordTypeMappings": [{
            "available": true,
            "defaultRecordTypeMapping": true,
            "layoutId": l["id"],
            "master": true,
            "name": "Master",
            "picklistsForRecordType": o.fields.iter().filter(|f| !f.picklist.is_empty()).map(|f| json!({
                "picklistName": f.name,
                "picklistValues": describe::field(o, f)["picklistValues"],
            })).collect::<Vec<_>>(),
            "recordTypeId": MASTER_RECORD_TYPE,
            "urls": record_type_urls(v, o),
        }],
        "recordTypeSelectorRequired": [false],
    })
}

/// The named layouts an object has: only User's alternate layout.
pub fn named_layouts(o: &SObject) -> &'static [&'static str] {
    if o.name == "User" { &["UserAlt"] } else { &[] }
}

/// `describe/compactLayouts`: the system default, which shows the name field.
pub fn compact_layouts(v: &str, o: &SObject) -> Value {
    let items: Vec<Value> = o.name_field().map(|f| field_item(o, &f.name, 1, false)).into_iter().collect();
    json!({
        "compactLayouts": [{
            "actions": [button("Edit"), button("Delete"), button("Clone")],
            "fieldItems": items,
            "id": null,
            "imageItems": [],
            "label": "System Default",
            "name": "SYSTEM",
            "objectType": o.name,
        }],
        "defaultCompactLayoutId": null,
        "recordTypeCompactLayoutMappings": [{
            "available": true,
            "compactLayoutId": null,
            "compactLayoutName": "SYSTEM",
            "recordTypeId": MASTER_RECORD_TYPE,
            "recordTypeName": "Master",
            "urls": {
                "compactLayout": format!("/services/data/v{v}/sobjects/{}/describe/compactLayouts/{MASTER_RECORD_TYPE}", o.name),
            },
        }],
    })
}

/// `describe/approvalLayouts`: localforce has no approval processes, so there are none.
pub fn approval_layouts() -> Value {
    json!({ "approvalLayouts": [] })
}
