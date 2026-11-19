//! The Tooling API (`/services/data/vXX.X/tooling/`), read-only.
//!
//! There's no Apex, so ApexClass, ApexTrigger and friends are always empty.
//! CustomObject, CustomField, EntityDefinition, FieldDefinition and
//! ValidationRule describe the org's schema: their rows are generated from
//! the catalog into temporary tables whenever they're queried, and queries
//! run through the ordinary SOQL engine. EntityDefinition and
//! FieldDefinition can be queried through the data API too, as in Salesforce.

use std::sync::OnceLock;

use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use serde_json::{Value, json};

use crate::describe;
use crate::error::{SfError, SfResult};
use crate::ids;
use crate::rest::{Req, Resp};
use crate::schema::{Catalog, Derived, Field, FieldType, SObject};
use crate::soql::exec::{Output, run_parsed};
use crate::store::{self, Ctx, qi};

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| Catalog::of(crate::schema::tooling_objects()))
}

pub fn route(conn: &Connection, ctx: &Ctx, req: &Req, rest: &[&str]) -> SfResult<Resp> {
    let tc = catalog();
    let v = &ctx.api_version;
    let read_only = || SfError::new("METHOD_NOT_ALLOWED", "localforce's Tooling API is read-only");
    match rest {
        [] => Ok(Resp::ok(json!({
            "sobjects": format!("/services/data/v{v}/tooling/sobjects"),
            "query": format!("/services/data/v{v}/tooling/query"),
            "executeAnonymous": format!("/services/data/v{v}/tooling/executeAnonymous"),
        }))),
        [kind @ ("query" | "queryAll")] => {
            let q = req
                .query
                .get("q")
                .ok_or_else(|| SfError::new("MALFORMED_QUERY", "A query string has to be specified"))?;
            let out = query(conn, &req.cat, ctx, q, false).unwrap_or_else(|| {
                let parsed = crate::soql::parse::parse(q)?;
                Err(SfError::new(
                    "INVALID_TYPE",
                    format!("sObject type '{}' is not supported by localforce's Tooling API.", parsed.from),
                ))
            })?;
            let entity = crate::soql::parse::parse(q).map(|p| p.from).unwrap_or_default();
            let entity = tc.object(&entity).map(|o| o.name.clone()).unwrap_or(entity);
            let _ = kind;
            Ok(Resp::ok(json!({
                "size": out.total_size,
                "totalSize": out.total_size,
                "done": true,
                "queryLocator": null,
                "entityTypeName": entity,
                "records": out.records,
            })))
        }
        ["sobjects"] => Ok(Resp::ok(tooling_urls(describe::global(v, tc)))),
        ["sobjects", obj] if req.method == axum::http::Method::GET => {
            let o = tc.object(obj).ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(tooling_urls(json!({ "objectDescribe": describe::global_entry(v, o), "recentItems": [] }))))
        }
        ["sobjects", obj, "describe"] => {
            let o = tc.object(obj).ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(tooling_urls(describe::sobject(v, tc, o))))
        }
        ["sobjects", obj, id] if req.method == axum::http::Method::GET => {
            let o = tc.object(obj).ok_or_else(SfError::not_found)?;
            refresh(conn, &req.cat)?;
            let fields: Option<Vec<String>> =
                req.query.get("fields").map(|f| f.split(',').map(|s| s.trim().to_string()).collect());
            Ok(Resp::ok(tooling_urls(store::get(conn, tc, ctx, o, id, fields.as_deref())?)))
        }
        ["sobjects", ..] => Err(read_only()),
        ["executeAnonymous"] => Ok(Resp::ok(json!({
            "line": -1,
            "column": -1,
            "compiled": false,
            "success": false,
            "compileProblem": "localforce doesn't run Apex",
            "exceptionStackTrace": null,
            "exceptionMessage": null,
        }))),
        _ => Err(SfError::not_found()),
    }
}

/// Run a query against the tooling objects, or `None` if it isn't for one.
/// From the data API (`data_api`), only EntityDefinition and FieldDefinition are queryable.
pub fn query(conn: &Connection, cat: &Catalog, ctx: &Ctx, q: &str, data_api: bool) -> Option<SfResult<Output>> {
    let parsed = crate::soql::parse::parse(q).ok()?;
    let tc = catalog();
    let o = tc.object(&parsed.from)?;
    if data_api && !matches!(o.name.as_str(), "EntityDefinition" | "FieldDefinition") {
        return None;
    }
    Some((|| {
        refresh(conn, cat)?;
        let mut out = run_parsed(conn, tc, ctx, q, &parsed, false, 2000)?;
        if !data_api {
            out.records = out.records.into_iter().map(tooling_urls).collect();
        }
        Ok(out)
    })())
}

/// Point record and describe URLs at `/tooling/sobjects/…`.
fn tooling_urls(v: Value) -> Value {
    match v {
        Value::String(s) if s.starts_with("/services/data/v") && s.contains("/sobjects") => {
            Value::String(s.replacen("/sobjects", "/tooling/sobjects", 1))
        }
        Value::Array(a) => Value::Array(a.into_iter().map(tooling_urls).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k, tooling_urls(v))).collect()),
        other => other,
    }
}

/// Generate the tooling tables' rows from the org's schema.
fn refresh(conn: &Connection, cat: &Catalog) -> SfResult<()> {
    for o in &catalog().objects {
        let cols: Vec<String> = o.fields.iter().map(|f| format!("{} {}", qi(&f.name), f.ty.sql_type())).collect();
        conn.execute_batch(&format!(
            "CREATE TEMP TABLE IF NOT EXISTS {0} ({1}, PRIMARY KEY (\"Id\")); DELETE FROM temp.{0};",
            qi(&o.name),
            cols.join(", ")
        ))?;
    }
    let text = |s: &str| Sql::Text(s.to_string());
    let opt = |s: &Option<String>| s.clone().map(Sql::Text).unwrap_or(Sql::Null);
    let int = |n: Option<u32>| n.map(|n| Sql::Integer(n as i64)).unwrap_or(Sql::Null);
    // Custom objects are known by their CustomObject id; standard ones by name.
    let durable = |i: usize, o: &SObject| if o.custom { ids::make("01I", i as u64 + 1) } else { o.name.clone() };
    let mut field_seq = 0;
    let mut rule_seq = 0;
    for (i, o) in cat.objects.iter().enumerate() {
        let entity = durable(i, o);
        let dev_name = o.name.trim_end_matches("__c").to_string();
        if o.custom {
            put(
                conn,
                "CustomObject",
                &[
                    ("Id", text(&entity)),
                    ("DeveloperName", text(&dev_name)),
                    ("Description", opt(&o.description)),
                    ("ManageableState", text("unmanaged")),
                ],
            )?;
        }
        put(
            conn,
            "EntityDefinition",
            &[
                ("Id", text(&ids::make("0Ed", i as u64 + 1))),
                ("DurableId", text(&entity)),
                ("QualifiedApiName", text(&o.name)),
                ("DeveloperName", text(&dev_name)),
                ("Label", text(&o.label)),
                ("PluralLabel", text(&o.label_plural)),
                ("KeyPrefix", text(&o.key_prefix)),
                ("IsCustomizable", Sql::Integer(1)),
                ("IsQueryable", Sql::Integer(1)),
                ("IsCustomSetting", Sql::Integer(0)),
            ],
        )?;
        for f in &o.fields {
            field_seq += 1;
            let field_dev = f.name.trim_end_matches("__c").to_string();
            if f.custom {
                put(
                    conn,
                    "CustomField",
                    &[
                        ("Id", text(&ids::make("00N", field_seq))),
                        ("DeveloperName", text(&field_dev)),
                        ("TableEnumOrId", text(&entity)),
                        ("Description", opt(&f.description)),
                    ],
                )?;
            }
            put(
                conn,
                "FieldDefinition",
                &[
                    ("Id", text(&ids::make("0Fd", field_seq))),
                    ("DurableId", text(&format!("{entity}.{}", f.name))),
                    ("EntityDefinitionId", text(&entity)),
                    ("QualifiedApiName", text(&f.name)),
                    ("DeveloperName", text(&field_dev)),
                    ("Label", text(&f.label)),
                    ("DataType", text(&data_type(f))),
                    ("Length", int(f.length.filter(|_| f.ty.is_text()))),
                    ("Precision", int(f.precision)),
                    ("Scale", int(f.scale)),
                    ("IsNillable", Sql::Integer(f.nillable() as i64)),
                    (
                        "IsCalculated",
                        Sql::Integer(matches!(f.derived, Derived::Formula(_) | Derived::Rollup(_)) as i64),
                    ),
                ],
            )?;
        }
        for r in &o.validation_rules {
            rule_seq += 1;
            put(
                conn,
                "ValidationRule",
                &[
                    ("Id", text(&ids::make("03d", rule_seq))),
                    ("ValidationName", text(&r.name)),
                    ("EntityDefinitionId", text(&entity)),
                    ("Active", Sql::Integer(r.active as i64)),
                    ("ErrorMessage", text(&r.message)),
                    ("ErrorDisplayField", opt(&r.field)),
                    ("Description", opt(&r.description)),
                ],
            )?;
        }
    }
    Ok(())
}

fn put(conn: &Connection, table: &str, values: &[(&str, Sql)]) -> SfResult<()> {
    let mut cols: Vec<String> = values.iter().map(|(c, _)| qi(c)).collect();
    let mut params: Vec<Sql> = values.iter().map(|(_, v)| v.clone()).collect();
    cols.push("\"IsDeleted\"".into());
    params.push(Sql::Integer(0));
    let marks: Vec<String> = (1..=params.len()).map(|i| format!("?{i}")).collect();
    conn.execute(
        &format!("INSERT INTO temp.{} ({}) VALUES ({})", qi(table), cols.join(", "), marks.join(", ")),
        rusqlite::params_from_iter(params),
    )?;
    Ok(())
}

/// The data type as Setup shows it, e.g. `Text(80)` or `Lookup(Account)`.
fn data_type(f: &Field) -> String {
    let num = |kind: &str| format!("{kind}({}, {})", f.precision.unwrap_or(18), f.scale.unwrap_or(2));
    let base = match f.ty {
        FieldType::Id => "Id".into(),
        FieldType::Reference if f.master_detail => {
            format!("Master-Detail({})", f.targets().collect::<Vec<_>>().join(","))
        }
        FieldType::Reference => format!("Lookup({})", f.targets().collect::<Vec<_>>().join(",")),
        FieldType::String if matches!(f.derived, Derived::AutoNumber(_)) => "Auto Number".into(),
        FieldType::String => format!("Text({})", f.length.unwrap_or(255)),
        FieldType::Textarea if f.length.unwrap_or(255) > 255 => format!("Long Text Area({})", f.length.unwrap_or(0)),
        FieldType::Textarea => "Text Area(255)".into(),
        FieldType::Email => "Email".into(),
        FieldType::Phone => "Phone".into(),
        FieldType::Url => "URL(255)".into(),
        FieldType::Picklist => "Picklist".into(),
        FieldType::Multipicklist => "Picklist (Multi-Select)".into(),
        FieldType::Boolean => "Checkbox".into(),
        FieldType::Int => "Number(9, 0)".into(),
        FieldType::Double => num("Number"),
        FieldType::Currency => num("Currency"),
        FieldType::Percent => num("Percent"),
        FieldType::Date => "Date".into(),
        FieldType::Datetime => "Date/Time".into(),
        FieldType::Address => "Address".into(),
    };
    match f.derived {
        Derived::Formula(_) => format!("Formula ({})", base.split('(').next().unwrap_or(&base).trim()),
        Derived::Rollup(_) => "Roll-Up Summary".into(),
        _ => base,
    }
}
