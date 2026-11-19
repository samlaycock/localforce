//! List views (`/sobjects/{type}/listviews`): the standard views a new org has
//! for each object ("All Accounts", "My Accounts", "New This Week", "Recently
//! Viewed Accounts"; "All" and "Recently Viewed" for custom objects), with their
//! describe and results.

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::error::{SfError, SfResult};
use crate::layouts::stable_id;
use crate::schema::{Catalog, Field, FieldType, SObject};
use crate::store::Ctx;

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Mine,
    NewThisWeek,
    Recent,
}

pub struct ListView {
    pub id: String,
    pub developer_name: String,
    pub label: String,
    filter: Filter,
    columns: Vec<String>,
}

/// The columns of a standard object's default views, as a new org has them.
fn default_columns(o: &SObject) -> Vec<&'static str> {
    match o.name.as_str() {
        "Account" => vec!["Name", "Site", "BillingState", "Phone", "Type", "Owner.Alias"],
        "Contact" => vec!["Name", "Account.Name", "Account.Site", "Phone", "Email", "Owner.Alias"],
        "Lead" => vec!["Name", "Company", "State", "Email", "Status", "CreatedDate", "Owner.Alias"],
        "Opportunity" => vec!["Name", "Account.Name", "Amount", "CloseDate", "StageName", "Owner.Alias"],
        "Case" => vec!["CaseNumber", "Contact.Name", "Subject", "Status", "Priority", "CreatedDate", "Owner.Alias"],
        "Campaign" => vec!["Name", "ParentId", "Type", "Status", "StartDate", "EndDate", "Owner.Alias"],
        "Contract" => vec!["ContractNumber", "Account.Name", "Status", "StartDate", "EndDate"],
        "Order" => vec!["OrderNumber", "Account.Name", "Status", "EffectiveDate", "TotalAmount"],
        "Product2" => vec!["Name", "ProductCode", "Description", "Family"],
        "Asset" => vec!["Name", "Account.Name", "Contact.Name", "Product2.Name", "SerialNumber", "Status"],
        "User" => vec!["Name", "Alias", "Username", "Profile.Name", "IsActive"],
        _ => vec!["Name"],
    }
}

pub fn for_object(cat: &Catalog, o: &SObject) -> Vec<ListView> {
    let columns: Vec<String> =
        default_columns(o).into_iter().filter(|c| resolve(cat, o, c).is_some()).map(String::from).collect();
    let columns = if columns.is_empty() {
        o.name_field().map(|f| vec![f.name.clone()]).unwrap_or_else(|| vec!["Id".into()])
    } else {
        columns
    };
    let plural = o.label_plural.clone();
    let compact = plural.replace(' ', "");
    let mut views: Vec<(String, String, Filter)> = if o.custom {
        vec![("All".into(), "All".into(), Filter::All)]
    } else {
        vec![(format!("All{compact}"), format!("All {plural}"), Filter::All)]
    };
    if o.field("OwnerId").is_some() && !o.custom {
        views.push((format!("My{compact}"), format!("My {plural}"), Filter::Mine));
    }
    if !o.custom {
        views.push(("NewThisWeek".into(), "New This Week".into(), Filter::NewThisWeek));
    }
    views.push((format!("RecentlyViewed{compact}"), format!("Recently Viewed {plural}"), Filter::Recent));
    views
        .into_iter()
        .map(|(developer_name, label, filter)| ListView {
            id: stable_id("00B", &format!("{}.{developer_name}", o.name)),
            developer_name,
            label,
            filter,
            columns: columns.clone(),
        })
        .collect()
}

/// Find a list view by id (15 or 18 characters) or developer name.
pub fn find(cat: &Catalog, o: &SObject, id: &str) -> SfResult<ListView> {
    let id18 = crate::ids::to18(id);
    for_object(cat, o)
        .into_iter()
        .find(|v| Some(&v.id) == id18.as_ref() || v.developer_name.eq_ignore_ascii_case(id))
        .ok_or_else(SfError::not_found)
}

/// Find a list view by id across every object, for `/query?explain={listViewId}`.
pub fn find_any<'a>(cat: &'a Catalog, id: &str) -> Option<(&'a SObject, ListView)> {
    let id18 = crate::ids::to18(id)?;
    if !id18.starts_with("00B") {
        return None;
    }
    cat.objects.iter().find_map(|o| for_object(cat, o).into_iter().find(|v| v.id == id18).map(|v| (o, v)))
}

/// Follow a column path (`Account.Name`) to its field.
fn resolve<'a>(cat: &'a Catalog, o: &'a SObject, path: &str) -> Option<&'a Field> {
    let mut obj = o;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return obj.field(part);
        }
        let f =
            obj.fields.iter().find(|f| f.relationship_name.as_deref().is_some_and(|r| r.eq_ignore_ascii_case(part)))?;
        obj = cat.object(f.reference_to.as_deref()?)?;
    }
    None
}

fn sort_labels(ty: FieldType) -> (&'static str, &'static str) {
    match ty {
        FieldType::Date | FieldType::Datetime => ("Old to New", "New to Old"),
        FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent => ("0-9", "9-0"),
        _ => ("A-Z", "Z-A"),
    }
}

/// Every column: the visible ones, then the hidden ones the results always carry.
fn columns(cat: &Catalog, o: &SObject, v: &ListView) -> Vec<Value> {
    let mut hidden = vec!["Id", "CreatedDate", "LastModifiedDate", "SystemModstamp"];
    if o.field("OwnerId").is_some() {
        hidden.insert(0, "Owner.Id");
    }
    let visible = v.columns.iter().map(|c| (c.as_str(), false));
    visible
        .chain(hidden.into_iter().filter(|h| !v.columns.iter().any(|c| c == h)).map(|h| (h, true)))
        .enumerate()
        .filter_map(|(i, (path, hidden))| {
            let f = resolve(cat, o, path)?;
            let (asc, desc) = sort_labels(f.ty);
            let first = i == 0;
            Some(json!({
                "ascendingLabel": asc,
                "descendingLabel": desc,
                "fieldNameOrPath": path,
                "hidden": hidden,
                "label": f.label,
                "searchable": f.ty.is_text(),
                "selectListItem": path,
                "sortDirection": if first { json!("ascending") } else { Value::Null },
                "sortIndex": if first { json!(0) } else { Value::Null },
                "sortable": f.sortable(),
                "type": f.ty.describe_name(),
            }))
        })
        .collect()
}

fn recent_ids(conn: &Connection, ctx: &Ctx, o: &SObject) -> SfResult<Vec<String>> {
    Ok(conn
        .prepare("SELECT id FROM _recent WHERE user = ?1 AND object = ?2 ORDER BY viewed DESC LIMIT 200")?
        .query_map(rusqlite::params![ctx.user_id, o.name], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

/// The view's query, as `describe` reports it.
pub fn query(conn: &Connection, cat: &Catalog, ctx: &Ctx, o: &SObject, v: &ListView) -> SfResult<String> {
    let cols: Vec<String> =
        columns(cat, o, v).iter().filter_map(|c| c["fieldNameOrPath"].as_str().map(String::from)).collect();
    let filter = match v.filter {
        Filter::All => None,
        Filter::Mine => Some(format!("OwnerId = '{}'", ctx.user_id)),
        Filter::NewThisWeek => Some("CreatedDate = THIS_WEEK".to_string()),
        Filter::Recent => {
            let ids = recent_ids(conn, ctx, o)?;
            Some(if ids.is_empty() {
                "Id = null".to_string()
            } else {
                format!("Id IN ({})", ids.iter().map(|i| format!("'{i}'")).collect::<Vec<_>>().join(", "))
            })
        }
    };
    let first = &v.columns[0];
    Ok(format!(
        "SELECT {} FROM {}{} ORDER BY {first} ASC NULLS FIRST, Id ASC NULLS FIRST",
        cols.join(", "),
        o.name,
        filter.map(|f| format!(" WHERE {f}")).unwrap_or_default()
    ))
}

fn urls(api: &str, o: &SObject, v: &ListView) -> (String, String, String) {
    let base = format!("/services/data/v{api}/sobjects/{}/listviews/{}", o.name, v.id);
    (base.clone(), format!("{base}/describe"), format!("{base}/results"))
}

pub fn entry(api: &str, o: &SObject, v: &ListView) -> Value {
    let (url, describe, results) = urls(api, o, v);
    json!({
        "describeUrl": describe,
        "developerName": v.developer_name,
        "id": v.id,
        "label": v.label,
        "resultsUrl": results,
        "soqlCompatible": true,
        "url": url,
    })
}

/// `GET /sobjects/{type}/listviews`.
pub fn list(api: &str, cat: &Catalog, o: &SObject) -> Value {
    let views: Vec<Value> = for_object(cat, o).iter().map(|v| entry(api, o, v)).collect();
    json!({
        "done": true,
        "listviews": views,
        "nextRecordsUrl": null,
        "size": views.len(),
        "sobjectType": o.name,
    })
}

/// `GET /sobjects/{type}/listviews/{id}/describe`.
pub fn describe(conn: &Connection, cat: &Catalog, ctx: &Ctx, o: &SObject, v: &ListView) -> SfResult<Value> {
    Ok(json!({
        "columns": columns(cat, o, v),
        "id": v.id,
        "orderBy": [
            { "fieldNameOrPath": v.columns[0], "nullsPosition": "first", "sortDirection": "ascending" },
            { "fieldNameOrPath": "Id", "nullsPosition": "first", "sortDirection": "ascending" },
        ],
        "query": query(conn, cat, ctx, o, v)?,
        "relatedEntityId": null,
        "scope": if v.filter == Filter::Mine { "mine" } else { "everything" },
        "scopeEntityId": null,
        "sobjectType": o.name,
        "whereCondition": { "conditions": [], "conjunction": "and" },
    }))
}

fn cell(record: &Value, path: &str) -> Value {
    let mut v = record;
    for part in path.split('.') {
        v = match v.get(part) {
            Some(next) => next,
            None => return Value::Null,
        };
    }
    match v {
        Value::Null => Value::Null,
        Value::String(s) => json!(s),
        other => json!(other.to_string()),
    }
}

/// `GET /sobjects/{type}/listviews/{id}/results?limit=&offset=`: values are strings.
pub fn results(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    o: &SObject,
    v: &ListView,
    limit: Option<usize>,
    offset: Option<usize>,
) -> SfResult<Value> {
    let cols = columns(cat, o, v);
    let limit = limit.unwrap_or(25).clamp(1, 2000);
    let offset = offset.unwrap_or(0);
    let soql = format!("{} LIMIT {limit} OFFSET {offset}", query(conn, cat, ctx, o, v)?);
    let out = crate::soql::run(conn, cat, ctx, &soql, false)?;
    let total = crate::soql::run(
        conn,
        cat,
        ctx,
        &format!("SELECT COUNT() FROM {}{}", o.name, {
            let q = query(conn, cat, ctx, o, v)?;
            q.find(" WHERE ").map(|i| q[i..q.find(" ORDER BY ").unwrap_or(q.len())].to_string()).unwrap_or_default()
        }),
        false,
    )?
    .total_size;
    let records: Vec<Value> = out
        .records
        .iter()
        .map(|r| {
            let cells: Vec<Value> = cols
                .iter()
                .map(|c| {
                    let path = c["fieldNameOrPath"].as_str().unwrap_or_default();
                    let mut m = Map::new();
                    m.insert("fieldNameOrPath".into(), json!(path));
                    m.insert("value".into(), cell(r, path));
                    Value::Object(m)
                })
                .collect();
            json!({ "columns": cells })
        })
        .collect();
    Ok(json!({
        "columns": cols,
        "developerName": v.developer_name,
        "done": offset + records.len() >= total,
        "id": v.id,
        "label": v.label,
        "records": records,
        "size": total,
    }))
}
