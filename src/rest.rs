//! The REST API under `/services/data/vXX.X/`.
//!
//! Everything goes through one dispatcher so that `/composite` can run its
//! subrequests through exactly the same code as top-level requests.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, Method, StatusCode};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::describe;
use crate::error::{SfError, SfResult};
use crate::schema::{Catalog, SObject};
use crate::soql;
use crate::state::{API_LIMIT, AppState, Cursor};
use crate::store::{self, Ctx, qi};

pub struct Req<'a> {
    pub method: Method,
    /// Path segments after `/services/data/vXX.X/`.
    pub segs: Vec<String>,
    pub query: HashMap<String, String>,
    pub body: Option<Value>,
    pub batch_size: usize,
    pub st: &'a AppState,
    /// The catalog as of the start of the request.
    pub cat: Arc<Catalog>,
    pub headers: HeaderMap,
}

impl Req<'_> {
    /// `https://host:port`, as the client reached localforce.
    pub fn base_url(&self) -> String {
        match self.headers.get("x-localforce-base-url").and_then(|h| h.to_str().ok()) {
            Some(b) => b.to_string(),
            None => {
                let host = self.headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("localhost");
                format!("http://{host}")
            }
        }
    }
}

pub struct Resp {
    pub status: StatusCode,
    pub body: Option<Value>,
    /// Undo the request's writes even though it succeeded (allOrNone failures report 200).
    pub rollback: bool,
    pub headers: Vec<(&'static str, String)>,
}

impl Resp {
    pub fn ok(body: Value) -> Self {
        Resp { status: StatusCode::OK, body: Some(body), rollback: false, headers: vec![] }
    }
    pub fn status(status: StatusCode, body: Value) -> Self {
        Resp { status, body: Some(body), rollback: false, headers: vec![] }
    }
    fn no_content() -> Self {
        Resp { status: StatusCode::NO_CONTENT, body: None, rollback: false, headers: vec![] }
    }
    fn not_modified() -> Self {
        Resp { status: StatusCode::NOT_MODIFIED, body: None, rollback: false, headers: vec![] }
    }
    fn last_modified(mut self, t: chrono::DateTime<chrono::Utc>) -> Self {
        self.headers.push(("last-modified", t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()));
        self
    }
    /// Whether the request's writes should be kept.
    pub fn commits(&self) -> bool {
        self.status.is_success() && !self.rollback
    }
}

impl From<SfError> for Resp {
    fn from(e: SfError) -> Self {
        Resp { status: e.status(), body: Some(json!([e.rest_json()])), rollback: true, headers: vec![] }
    }
}

/// `If-Modified-Since`, to the second.
fn modified_since(req: &Req) -> Option<chrono::DateTime<chrono::Utc>> {
    let h = req.headers.get("if-modified-since")?.to_str().ok()?;
    // The weekday is redundant, so a wrong one is ignored.
    let date = h.split_once(", ").map_or(h, |(_, d)| d).trim_end_matches(" GMT");
    chrono::NaiveDateTime::parse_from_str(date, "%d %b %Y %H:%M:%S").ok().map(|t| t.and_utc())
}

fn unchanged_since(req: &Req, changed: chrono::DateTime<chrono::Utc>) -> bool {
    modified_since(req).is_some_and(|since| changed.timestamp() <= since.timestamp())
}

/// The API versions served. 21.0–30.0 were retired in Summer '25; the newest
/// is the latest release's. Requests for anything else are NOT_FOUND.
pub const API_VERSIONS: std::ops::RangeInclusive<u32> = 31..=66;

pub fn supported_version(v: &str) -> bool {
    v.split_once('.')
        .is_some_and(|(major, minor)| minor == "0" && major.parse::<u32>().is_ok_and(|n| API_VERSIONS.contains(&n)))
}

pub fn versions() -> Value {
    API_VERSIONS
        .map(|n| json!({ "label": format!("API {n}.0"), "url": format!("/services/data/v{n}.0"), "version": format!("{n}.0") }))
        .collect()
}

fn not_allowed(m: &Method, allowed: &str) -> SfError {
    SfError::new("METHOD_NOT_ALLOWED", format!("HTTP Method '{m}' not allowed. Allowed are {allowed}"))
}

fn body_object(req: &Req) -> SfResult<Map<String, Value>> {
    match &req.body {
        Some(Value::Object(m)) => Ok(m.clone()),
        _ => Err(SfError::new("JSON_PARSER_ERROR", "The request body is either invalid or incomplete")),
    }
}

/// Dispatch one request. The caller holds the database lock and wraps the
/// call in a savepoint, so a failed request leaves nothing behind.
pub fn dispatch(conn: &Connection, ctx: &Ctx, req: Req) -> Resp {
    match route(conn, ctx, &req) {
        Ok(r) => r,
        Err(e) => e.into(),
    }
}

fn route(conn: &Connection, ctx: &Ctx, req: &Req) -> SfResult<Resp> {
    let st = req.st;
    let cat = &*req.cat;
    let v = &ctx.api_version;
    let segs: Vec<&str> = req.segs.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
    let m = &req.method;
    match segs.as_slice() {
        [] => Ok(Resp::ok(json!({
            "tooling": format!("/services/data/v{v}/tooling"),
            "tabs": format!("/services/data/v{v}/tabs"),
            "identity": format!("{}/id/{}/{}", req.base_url(), st.org_id, ctx.user_id),
            "theme": format!("/services/data/v{v}/theme"),
            "jobs": format!("/services/data/v{v}/jobs"),
            "sobjects": format!("/services/data/v{v}/sobjects"),
            "query": format!("/services/data/v{v}/query"),
            "queryAll": format!("/services/data/v{v}/queryAll"),
            "search": format!("/services/data/v{v}/search"),
            "parameterizedSearch": format!("/services/data/v{v}/parameterizedSearch"),
            "recent": format!("/services/data/v{v}/recent"),
            "composite": format!("/services/data/v{v}/composite"),
            "limits": format!("/services/data/v{v}/limits"),
        }))),
        ["limits"] => {
            let used = st.api_calls.load(std::sync::atomic::Ordering::Relaxed);
            Ok(Resp::ok(limits(used)))
        }
        ["tabs"] if m == Method::GET => Ok(Resp::ok(json!(crate::tabs::tabs(&req.base_url(), cat)))),
        ["theme"] if m == Method::GET => Ok(Resp::ok(crate::tabs::theme(&req.base_url(), cat))),
        ["sobjects"] if m == Method::GET => {
            let changed = *st.schema_changed.read().unwrap();
            if unchanged_since(req, changed) {
                return Ok(Resp::not_modified());
            }
            Ok(Resp::ok(describe::global(v, cat)).last_modified(changed))
        }
        ["sobjects", obj] => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            match *m {
                Method::GET => Ok(Resp::ok(json!({
                    "objectDescribe": describe::global_entry(v, o),
                    "recentItems": recent(conn, cat, ctx, Some(&o.name), 3)?,
                }))),
                Method::POST => {
                    let id = store::insert(conn, cat, ctx, o, &body_object(req)?)?;
                    Ok(Resp::status(StatusCode::CREATED, json!({ "id": id, "success": true, "errors": [] })))
                }
                _ => Err(not_allowed(m, "GET,HEAD,POST")),
            }
        }
        ["sobjects", obj, "describe"] => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let changed = *st.schema_changed.read().unwrap();
            if unchanged_since(req, changed) {
                return Ok(Resp::not_modified());
            }
            Ok(Resp::ok(describe::sobject(v, cat, o)).last_modified(changed))
        }
        ["sobjects", obj, "describe", "layouts", rest @ ..] if m == Method::GET && rest.len() <= 1 => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            match rest {
                [] => Ok(Resp::ok(crate::layouts::layouts(v, cat, o, None))),
                [rt] if crate::ids::to18(rt).as_deref() == Some(crate::layouts::MASTER_RECORD_TYPE) => {
                    Ok(Resp::ok(crate::layouts::layout(cat, o, None)))
                }
                _ => Err(SfError::not_found()),
            }
        }
        ["sobjects", obj, "describe", "namedLayouts", name] if m == Method::GET => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let name = crate::layouts::named_layouts(o)
                .iter()
                .find(|n| n.eq_ignore_ascii_case(name))
                .ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(crate::layouts::layouts(v, cat, o, Some(name))))
        }
        ["sobjects", obj, "describe", "compactLayouts"] if m == Method::GET => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(crate::layouts::compact_layouts(v, o)))
        }
        ["sobjects", obj, "describe", "approvalLayouts", ..] if m == Method::GET => {
            cat.object(obj).ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(crate::layouts::approval_layouts()))
        }
        ["sobjects", obj, "listviews"] if m == Method::GET => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            Ok(Resp::ok(crate::listviews::list(v, cat, o)))
        }
        ["sobjects", obj, "listviews", id, rest @ ..] if m == Method::GET && rest.len() <= 1 => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let view = crate::listviews::find(cat, o, id)?;
            let num = |k: &str| req.query.get(k).and_then(|n| n.parse::<usize>().ok());
            match rest {
                [] => Ok(Resp::ok(crate::listviews::entry(v, o, &view))),
                ["describe"] => Ok(Resp::ok(crate::listviews::describe(conn, cat, ctx, o, &view)?)),
                ["results"] => {
                    Ok(Resp::ok(crate::listviews::results(conn, cat, ctx, o, &view, num("limit"), num("offset"))?))
                }
                _ => Err(SfError::not_found()),
            }
        }
        ["sobjects", obj, which @ ("updated" | "deleted")] if m == Method::GET => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            replication(conn, ctx, req, o, *which == "deleted")
        }
        ["sobjects", obj, id] => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            match *m {
                Method::GET => {
                    let fields: Option<Vec<String>> =
                        req.query.get("fields").map(|f| f.split(',').map(|s| s.trim().to_string()).collect());
                    let rec = store::get(conn, cat, ctx, o, id, fields.as_deref())?;
                    let id18 = crate::ids::to18(id).unwrap_or_default();
                    let modified: Option<String> = conn
                        .query_row(
                            &format!("SELECT \"LastModifiedDate\" FROM {} WHERE \"Id\" = ?1", qi(&o.name)),
                            [&id18],
                            |r| r.get(0),
                        )
                        .ok();
                    let modified = modified.as_deref().and_then(store::parse_datetime);
                    match modified {
                        Some(t) if unchanged_since(req, t) => Ok(Resp::not_modified()),
                        Some(t) => Ok(Resp::ok(rec).last_modified(t)),
                        None => Ok(Resp::ok(rec)),
                    }
                }
                Method::PATCH => {
                    store::update(conn, cat, ctx, o, id, &body_object(req)?)?;
                    Ok(Resp::no_content())
                }
                Method::DELETE => {
                    store::delete(conn, cat, ctx, o, id)?;
                    Ok(Resp::no_content())
                }
                _ => Err(not_allowed(m, "GET,HEAD,PATCH,DELETE")),
            }
        }
        ["sobjects", obj, field, value] => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let f = o.field(field).filter(|f| f.external_id || f.name == "Id").ok_or_else(|| {
                SfError::new(
                    "NOT_FOUND",
                    format!("Provided external ID field does not exist or is not accessible: {field}"),
                )
            })?;
            let found = store::find_by(conn, o, f, value)?;
            match (m.clone(), found.as_slice()) {
                (_, [_, _, ..]) => Ok(Resp::status(
                    StatusCode::MULTIPLE_CHOICES,
                    json!(found.iter().map(|id| store::record_url(ctx, &o.name, id)).collect::<Vec<_>>()),
                )),
                (Method::GET, [id]) => Ok(Resp::ok(store::get(conn, cat, ctx, o, id, None)?)),
                (Method::DELETE, [id]) => {
                    store::delete(conn, cat, ctx, o, id)?;
                    Ok(Resp::no_content())
                }
                (Method::PATCH, [id]) => {
                    store::update(conn, cat, ctx, o, id, &body_object(req)?)?;
                    Ok(Resp::ok(json!({ "id": id, "success": true, "errors": [], "created": false })))
                }
                (Method::PATCH, []) => {
                    let mut body = body_object(req)?;
                    if f.name != "Id" {
                        body.insert(f.name.clone(), Value::String(value.to_string()));
                    }
                    let id = store::insert(conn, cat, ctx, o, &body)?;
                    Ok(Resp::status(
                        StatusCode::CREATED,
                        json!({ "id": id, "success": true, "errors": [], "created": true }),
                    ))
                }
                (Method::GET | Method::DELETE, []) => Err(SfError::not_found()),
                _ => Err(not_allowed(m, "GET,HEAD,PATCH,DELETE")),
            }
        }
        [kind @ ("query" | "queryAll")] if req.query.contains_key("explain") => {
            let target = &req.query["explain"];
            let soql = match crate::listviews::find_any(cat, target.trim()) {
                Some((o, view)) => crate::listviews::query(conn, cat, ctx, o, &view)?,
                None => target.clone(),
            };
            Ok(Resp::ok(crate::explain::plans(conn, cat, ctx, &soql)?))
        }
        [kind @ ("query" | "queryAll")] => {
            let q = req
                .query
                .get("q")
                .ok_or_else(|| SfError::new("MALFORMED_QUERY", "A query string has to be specified"))?;
            let out = match soql::run(conn, cat, ctx, q, *kind == "queryAll") {
                // EntityDefinition and friends are queryable through the data API too.
                Err(e) if e.code == "INVALID_TYPE" => match crate::tooling::query(conn, cat, ctx, q, true) {
                    Some(r) => r?,
                    None => return Err(e),
                },
                other => other?,
            };
            let cursor = Cursor {
                records: Arc::new(out.records),
                total_size: out.total_size,
                touched: std::time::Instant::now(),
            };
            Ok(Resp::ok(page(st, ctx, kind, None, cursor, 0, req.batch_size)))
        }
        [kind @ ("query" | "queryAll"), locator] => {
            let (id, offset) = locator
                .rsplit_once('-')
                .ok_or_else(|| SfError::new("INVALID_QUERY_LOCATOR", "invalid query locator"))?;
            let offset: usize =
                offset.parse().map_err(|_| SfError::new("INVALID_QUERY_LOCATOR", "invalid query locator"))?;
            let cursor = st.cursor(id).ok_or_else(|| SfError::new("INVALID_QUERY_LOCATOR", "invalid query locator"))?;
            Ok(Resp::ok(page(st, ctx, kind, Some(id.to_string()), cursor, offset, req.batch_size)))
        }
        ["search"] if m == Method::GET => {
            let q = req.query.get("q").ok_or_else(|| {
                SfError::new("MALFORMED_SEARCH", "No search term found. The search term must be enclosed in braces.")
            })?;
            Ok(Resp::ok(crate::sosl::search(conn, cat, ctx, q)?))
        }
        ["parameterizedSearch"] if m == Method::GET || m == Method::POST => {
            let sosl = match &req.body {
                Some(b) if m == Method::POST => crate::sosl::from_json(b)?,
                _ => crate::sosl::from_params(&req.query)?,
            };
            Ok(Resp::ok(crate::sosl::search(conn, cat, ctx, &sosl)?))
        }
        ["recent"] if m == Method::GET => {
            let limit = req.query.get("limit").and_then(|l| l.parse::<usize>().ok()).unwrap_or(200).min(200);
            Ok(Resp::ok(json!(recent(conn, cat, ctx, None, limit)?)))
        }
        ["tooling", rest @ ..] => crate::tooling::route(conn, ctx, req, rest),
        ["composite"] if m == Method::POST => composite(conn, ctx, req),
        ["composite", "tree", obj] if m == Method::POST => tree(conn, ctx, req, obj),
        ["composite", "batch"] if m == Method::POST => batch(conn, ctx, req),
        ["composite", "graph"] if m == Method::POST => graph(conn, ctx, req),
        ["composite", "sobjects"] => {
            let op = match *m {
                Method::POST => Op::Create,
                Method::PATCH => Op::Update,
                Method::DELETE => Op::Delete,
                _ => return Err(not_allowed(m, "POST,PATCH,DELETE")),
            };
            collections(conn, ctx, req, op)
        }
        // Retrieve several records: GET with ?ids=&fields=, or POST { ids, fields } for long id lists.
        ["composite", "sobjects", obj] if m == Method::GET || m == Method::POST => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let (ids, fields): (Vec<String>, Option<Vec<String>>) = if m == Method::GET {
                let ids = req
                    .query
                    .get("ids")
                    .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                    .unwrap_or_default();
                let fields = req.query.get("fields").map(|f| f.split(',').map(|s| s.trim().to_string()).collect());
                (ids, fields)
            } else {
                let body = body_object(req)?;
                let strings = |k: &str| {
                    body.get(k)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>())
                };
                (strings("ids").unwrap_or_default(), strings("fields"))
            };
            let fields =
                fields.ok_or_else(|| SfError::new("MISSING_ARGUMENT", "fields: The fields parameter is required"))?;
            if ids.len() > 2000 {
                return Err(SfError::new("EXCEEDED_ID_LIMIT", "Number of ids exceeded the limit of 2000"));
            }
            let recs: Vec<Value> = ids
                .iter()
                .filter(|s| !s.is_empty())
                .map(|id| match store::get(conn, cat, ctx, o, id, Some(&fields)) {
                    Ok(v) => Ok(v),
                    Err(e) if matches!(e.code, "NOT_FOUND" | "ENTITY_IS_DELETED") => Ok(Value::Null),
                    Err(e) => Err(e),
                })
                .collect::<SfResult<_>>()?;
            Ok(Resp::ok(json!(recs)))
        }
        // Upsert several records of one type by an external id.
        ["composite", "sobjects", obj, field] if m == Method::PATCH => {
            let o = cat.object(obj).ok_or_else(SfError::not_found)?;
            let f = o.field(field).filter(|f| f.external_id || f.name == "Id").ok_or_else(|| {
                SfError::new(
                    "INVALID_FIELD",
                    format!("Field name provided, {field} is not an External ID or indexed field for {}", o.name),
                )
            })?;
            collections(conn, ctx, req, Op::Upsert { object: o.name.clone(), field: f.name.clone() })
        }
        _ => Err(SfError::not_found()),
    }
}

/// One page of query results. The rest stay under a locator (`id`, or a new one).
pub fn page(
    st: &AppState,
    ctx: &Ctx,
    kind: &str,
    id: Option<String>,
    cursor: Cursor,
    offset: usize,
    batch: usize,
) -> Value {
    let end = (offset + batch).min(cursor.records.len());
    let records: Vec<Value> = cursor.records[offset.min(end)..end].to_vec();
    let total = cursor.total_size;
    let more = end < cursor.records.len();
    let mut out = json!({ "totalSize": total, "done": !more, "records": records });
    if more {
        let id = id.unwrap_or_else(|| st.store_cursor(cursor));
        out["nextRecordsUrl"] = json!(format!("/services/data/v{}/{kind}/{id}-{end}", ctx.api_version));
    }
    out
}

/// Records the user viewed (or referenced) most recently, newest first: from
/// SOQL's `FOR VIEW` and `FOR REFERENCE`.
fn recent(conn: &Connection, cat: &Catalog, ctx: &Ctx, object: Option<&str>, limit: usize) -> SfResult<Vec<Value>> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT object, id FROM _recent WHERE user = ?1 AND (?2 IS NULL OR object = ?2) ORDER BY viewed DESC")?
        .query_map(rusqlite::params![ctx.user_id, object], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (object, id) in rows {
        let Some(o) = cat.object(&object) else { continue };
        let name_col = o.name_field().map(|f| qi(&f.name)).unwrap_or_else(|| "NULL".into());
        let name: Option<Option<String>> = conn
            .query_row(
                &format!("SELECT {name_col} FROM {} WHERE \"Id\" = ?1 AND \"IsDeleted\" = 0", qi(&o.name)),
                [&id],
                |r| r.get(0),
            )
            .ok();
        let Some(name) = name else { continue };
        let mut rec = Map::new();
        rec.insert("attributes".into(), store::attributes(ctx, &o.name, Some(&id)));
        rec.insert("Id".into(), json!(id));
        if let Some(f) = o.name_field() {
            rec.insert(f.name.clone(), json!(name));
        }
        out.push(Value::Object(rec));
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// `sobjects/{type}/updated` and `/deleted`: what changed between `start` and `end`, for replication.
fn replication(conn: &Connection, ctx: &Ctx, req: &Req, o: &SObject, deleted: bool) -> SfResult<Resp> {
    let param = |name: &str| -> SfResult<chrono::DateTime<chrono::Utc>> {
        let raw = req
            .query
            .get(name)
            .ok_or_else(|| SfError::new("MISSING_ARGUMENT", format!("{name}: The {name} parameter is required")))?;
        // A `+` in the offset often arrives as a space.
        store::parse_datetime(&raw.trim().replace(' ', "+"))
            .ok_or_else(|| SfError::new("INVALID_REPLICATION_DATE", format!("{name}: invalid date: {raw}")))
    };
    let to_minute = |t: chrono::DateTime<chrono::Utc>| {
        use chrono::Timelike;
        t.with_second(0).and_then(|t| t.with_nanosecond(0)).unwrap_or(t)
    };
    let (start, end) = (param("start")?, param("end")?);
    if end <= start {
        return Err(SfError::new("INVALID_REPLICATION_DATE", "start date must chronologically precede end date"));
    }
    let earliest = to_minute(ctx.now - chrono::Duration::days(30));
    if start < earliest {
        return Err(SfError::new("INVALID_REPLICATION_DATE", "startDate cannot be more than 30 days ago"));
    }
    let latest = to_minute(end.min(ctx.now));
    let sql = format!(
        "SELECT \"Id\", \"SystemModstamp\" FROM {} WHERE \"IsDeleted\" = ?1 AND \"SystemModstamp\" >= ?2 AND \"SystemModstamp\" < ?3 ORDER BY \"SystemModstamp\"",
        qi(&o.name)
    );
    let rows: Vec<(String, String)> = conn
        .prepare(&sql)?
        .query_map(
            rusqlite::params![deleted, store::format_datetime(to_minute(start)), store::format_datetime(end)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
        .collect::<Result<_, _>>()?;
    let latest = store::format_datetime(latest);
    Ok(Resp::ok(if deleted {
        json!({
            "deletedRecords": rows.iter().map(|(id, at)| json!({ "id": id, "deletedDate": at })).collect::<Vec<_>>(),
            "earliestDateAvailable": store::format_datetime(earliest),
            "latestDateCovered": latest,
        })
    } else {
        json!({ "ids": rows.iter().map(|(id, _)| id).collect::<Vec<_>>(), "latestDateCovered": latest })
    }))
}

/// Records for a collection call: the record and its type, or why it can't be processed.
type CollectionItems = Vec<Result<(Map<String, Value>, String), String>>;

const ROLLED_BACK: &str =
    "Record rolled back because not all records were valid and the request was using AllOrNone header";

/// What an sObject Collections call does to each record.
#[derive(Clone)]
pub enum Op {
    Create,
    Update,
    Delete,
    Upsert { object: String, field: String },
}

/// A field of a request record, matched case-insensitively like Salesforce.
fn get_ci<'v>(rec: &'v Map<String, Value>, name: &str) -> Option<&'v Value> {
    rec.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

/// sObject Collections: up to 200 records of mixed types in one call.
fn collections(conn: &Connection, ctx: &Ctx, req: &Req, op: Op) -> SfResult<Resp> {
    let cat = &*req.cat;
    let (all_or_none, items): (bool, CollectionItems) = match op {
        Op::Delete => {
            let all = req.query.get("allOrNone").is_some_and(|v| v == "true");
            let ids = req.query.get("ids").cloned().unwrap_or_default();
            let ids: Vec<&str> = ids.split(',').filter(|s| !s.is_empty()).collect();
            if ids.len() > 200 {
                return Err(SfError::new(
                    "EXCEEDED_ID_LIMIT",
                    "record limit reached. cannot submit more than 200 records into this call",
                ));
            }
            let items = ids
                .into_iter()
                .map(|id| match cat.object_for_id(id) {
                    Some(o) => Ok((Map::from_iter([("Id".to_string(), Value::String(id.into()))]), o.name.clone())),
                    None => Err(format!("malformed id {id}")),
                })
                .collect();
            (all, items)
        }
        _ => {
            let body = body_object(req)?;
            let all = body.get("allOrNone").and_then(Value::as_bool).unwrap_or(false);
            let recs = body.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
            if recs.len() > 200 {
                return Err(SfError::new(
                    "EXCEEDED_ID_LIMIT",
                    "record limit reached. cannot submit more than 200 records into this call",
                ));
            }
            let items = recs
                .into_iter()
                .map(|r| {
                    let r = r.as_object().cloned().unwrap_or_default();
                    let ty = r.get("attributes").and_then(|a| a.get("type")).and_then(Value::as_str).map(String::from);
                    match (ty, &op) {
                        (Some(t), _) => Ok((r, t)),
                        (None, Op::Upsert { object, .. }) => Ok((r, object.clone())),
                        (None, _) => Err("No SObject Type defined in the record".to_string()),
                    }
                })
                .collect();
            (all, items)
        }
    };

    // (id, created) per record, and whether an upsert found no record to update.
    // Salesforce reports `created: true` for those even when the save fails.
    let mut results: Vec<SfResult<(String, bool)>> = vec![];
    let mut inserts: Vec<bool> = vec![];
    let mut failed = false;
    for (i, item) in items.into_iter().enumerate() {
        let sp = format!("lf_item_{i}");
        conn.execute_batch(&format!("SAVEPOINT {sp}"))?;
        let mut would_create = false;
        let r: SfResult<(String, bool)> = (|| {
            let (rec, ty) = item.map_err(|m| SfError::new("INVALID_FIELD", m))?;
            let o = cat
                .object(&ty)
                .ok_or_else(|| SfError::new("INVALID_TYPE", format!("sObject type '{ty}' is not supported.")))?;
            let id = get_ci(&rec, "Id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
            let normal = |id: String| crate::ids::to18(&id).unwrap_or(id);
            match &op {
                Op::Create => Ok((store::insert(conn, cat, ctx, o, &rec)?, true)),
                Op::Update => {
                    let id =
                        id.ok_or_else(|| SfError::new("MISSING_ARGUMENT", "Id not specified in an update call"))?;
                    store::update(conn, cat, ctx, o, &id, &rec)?;
                    Ok((normal(id), false))
                }
                Op::Delete => {
                    let id = id.unwrap();
                    store::delete(conn, cat, ctx, o, &id)?;
                    Ok((normal(id), false))
                }
                Op::Upsert { field, .. } => {
                    let f = o.field(field).unwrap();
                    let key = get_ci(&rec, field).cloned().unwrap_or(Value::Null);
                    let key = match key {
                        Value::String(s) if !s.is_empty() => s,
                        Value::Number(n) => n.to_string(),
                        _ => {
                            return Err(SfError::new("MISSING_ARGUMENT", format!("{field} not specified"))
                                .with_fields(vec![field.clone()]));
                        }
                    };
                    match store::find_by(conn, o, f, &key)?.as_slice() {
                        [] => {
                            would_create = true;
                            Ok((store::insert(conn, cat, ctx, o, &rec)?, true))
                        }
                        [id] => {
                            let mut body = rec.clone();
                            body.retain(|k, _| !k.eq_ignore_ascii_case(field));
                            store::update(conn, cat, ctx, o, id, &body)?;
                            Ok((id.clone(), false))
                        }
                        many => Err(SfError::new(
                            "DUPLICATE_EXTERNAL_ID",
                            format!("{field}: more than one record found for external id field: {}", many.join(", ")),
                        )
                        .with_fields(vec![field.clone()])),
                    }
                }
            }
        })();
        inserts.push(would_create);
        match r {
            Ok(ok) => {
                conn.execute_batch(&format!("RELEASE {sp}"))?;
                results.push(Ok(ok));
            }
            Err(e) => {
                conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp}"))?;
                failed = true;
                results.push(Err(e));
            }
        }
    }
    let rollback = all_or_none && failed;
    let upsert = matches!(op, Op::Upsert { .. });
    let body: Vec<Value> = results
        .into_iter()
        .zip(inserts)
        .map(|(r, would_create)| {
            let mut v = match r {
                Ok(_) if rollback => json!({ "id": null, "success": false, "errors": [{ "statusCode": "ALL_OR_NONE_OPERATION_ROLLED_BACK", "message": ROLLED_BACK, "fields": [] }] }),
                Ok((id, created)) => {
                    let mut v = json!({ "id": id, "success": true, "errors": [] });
                    if upsert {
                        v["created"] = json!(created);
                    }
                    v
                }
                Err(e) => json!({ "id": null, "success": false, "errors": [e.save_result_json()] }),
            };
            if upsert && v["success"] == false {
                v["created"] = json!(would_create);
            }
            v
        })
        .collect();
    Ok(Resp { rollback, ..Resp::ok(json!(body)) })
}

/// `/composite`: up to 25 subrequests, later ones referring to earlier results with `@{ref.field}`.
fn composite(conn: &Connection, ctx: &Ctx, req: &Req) -> SfResult<Resp> {
    let body = body_object(req)?;
    let all_or_none = body.get("allOrNone").and_then(Value::as_bool).unwrap_or(false);
    let subs = body.get("compositeRequest").and_then(Value::as_array).cloned().unwrap_or_default();
    if subs.len() > 25 {
        return Err(SfError::new("INVALID_BATCH_REQUEST", "Composite request can contain at most 25 subrequests"));
    }
    let mut seen = std::collections::HashSet::new();
    for sub in &subs {
        let r = sub.get("referenceId").and_then(Value::as_str).unwrap_or_default();
        if !seen.insert(r) {
            return Err(SfError::new(
                "JSON_PARSER_ERROR",
                format!("Duplicate ReferenceId provided in the request: {r}"),
            ));
        }
    }
    let mut results: Vec<(String, Resp)> = vec![];
    let mut refs: HashMap<String, Value> = HashMap::new();
    let mut halted = false;
    for (i, sub) in subs.iter().enumerate() {
        let ref_id = sub.get("referenceId").and_then(Value::as_str).unwrap_or_default().to_string();
        if halted {
            results.push((ref_id, halted_error().into()));
            continue;
        }
        let resolve = |s: &str| substitute(s, &refs);
        let url = resolve(sub.get("url").and_then(Value::as_str).unwrap_or_default());
        let method =
            sub.get("method").and_then(Value::as_str).unwrap_or("GET").parse::<Method>().unwrap_or(Method::GET);
        let sub_body = sub.get("body").map(|b| substitute_json(b, &refs));
        let (path, qs) = url.split_once('?').unwrap_or((&url, ""));
        let prefix = format!("/services/data/v{}/", ctx.api_version);
        let Some(rest) = path.strip_prefix(&prefix) else {
            results.push((
                ref_id,
                SfError::new("NOT_FOUND", format!("The requested resource does not exist: {path}")).into(),
            ));
            halted = all_or_none;
            continue;
        };
        let sp = format!("lf_sub_{i}");
        conn.execute_batch(&format!("SAVEPOINT {sp}"))?;
        let resp = dispatch(
            conn,
            ctx,
            Req {
                method,
                segs: rest.split('/').map(url_decode).collect(),
                query: parse_query(qs),
                body: sub_body,
                batch_size: req.batch_size,
                st: req.st,
                cat: req.cat.clone(),
                headers: sub_headers(sub.get("httpHeaders")),
            },
        );
        if resp.commits() {
            conn.execute_batch(&format!("RELEASE {sp}"))?;
            if let Some(b) = &resp.body {
                refs.insert(ref_id.clone(), b.clone());
            }
        } else {
            conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp}"))?;
            halted = all_or_none;
        }
        results.push((ref_id, resp));
    }
    if halted {
        // The whole transaction was rolled back, so nothing that "succeeded" stands.
        for (_, resp) in results.iter_mut().filter(|(_, r)| r.commits()) {
            *resp = halted_error().into();
        }
    }
    let body = json!({
        "compositeResponse": results.into_iter().map(|(r, resp)| json!({
            "body": resp.body,
            "httpHeaders": {},
            "httpStatusCode": resp.status.as_u16(),
            "referenceId": r,
        })).collect::<Vec<_>>()
    });
    Ok(Resp { rollback: halted, ..Resp::ok(body) })
}

/// A subrequest's `httpHeaders`.
fn sub_headers(v: Option<&Value>) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in v.and_then(Value::as_object).into_iter().flatten() {
        if let (Ok(name), Some(Ok(value))) =
            (k.parse::<axum::http::HeaderName>(), v.as_str().map(axum::http::HeaderValue::from_str))
        {
            h.insert(name, value);
        }
    }
    h
}

/// `/composite/tree/{type}`: records with their children, all created or none.
fn tree(conn: &Connection, ctx: &Ctx, req: &Req, obj: &str) -> SfResult<Resp> {
    let cat = &*req.cat;
    let o = cat.object(obj).ok_or_else(SfError::not_found)?;
    let body = body_object(req)?;
    let records = body
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| SfError::new("JSON_PARSER_ERROR", "records: The records property is required"))?;
    fn count(recs: &[Value]) -> usize {
        recs.iter()
            .map(|r| {
                1 + r
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(_, v)| v.get("records").and_then(Value::as_array))
                    .map(|c| count(c))
                    .sum::<usize>()
            })
            .sum()
    }
    if count(records) > 200 {
        return Err(SfError::new("LIMIT_EXCEEDED", "Too many records: a tree request can create at most 200"));
    }

    let mut t = crate::tree::Tree::new(conn, cat, ctx);
    for r in records {
        if let Err(e) = t.insert(r, o, None, 1) {
            return Ok(Resp::status(StatusCode::BAD_REQUEST, json!({ "hasErrors": true, "results": [e] })));
        }
    }
    let results: Vec<_> =
        t.ids.into_iter().map(|(reference, id)| json!({ "referenceId": reference, "id": id })).collect();
    Ok(Resp::status(StatusCode::CREATED, json!({ "hasErrors": false, "results": results })))
}

/// A subrequest URL (`v63.0/sobjects/Account` or `/services/data/v63.0/…`): its version, path and query.
fn sub_url(url: &str) -> Option<(String, String, String)> {
    let url = url.trim_start_matches('/');
    let url = url.strip_prefix("services/data/").unwrap_or(url);
    let (version, rest) = url.strip_prefix('v')?.split_once('/').unwrap_or((url.strip_prefix('v')?, ""));
    if !supported_version(version) {
        return None;
    }
    let (path, qs) = rest.split_once('?').unwrap_or((rest, ""));
    Some((version.to_string(), path.to_string(), qs.to_string()))
}

/// `/composite/batch`: up to 25 independent subrequests, each committed on its own.
fn batch(conn: &Connection, ctx: &Ctx, req: &Req) -> SfResult<Resp> {
    let body = body_object(req)?;
    let halt_on_error = body.get("haltOnError").and_then(Value::as_bool).unwrap_or(false);
    let subs = body.get("batchRequests").and_then(Value::as_array).cloned().unwrap_or_default();
    if subs.len() > 25 {
        return Err(SfError::new("INVALID_BATCH_REQUEST", "A batch can contain at most 25 subrequests"));
    }
    let mut results = vec![];
    let mut halted = false;
    for (i, sub) in subs.iter().enumerate() {
        if halted {
            results.push(json!({
                "statusCode": 412,
                "result": [{ "errorCode": "BATCH_PROCESSING_HALTED", "message": "Batch processing halted per request" }],
            }));
            continue;
        }
        let url = sub.get("url").and_then(Value::as_str).unwrap_or_default();
        let method =
            sub.get("method").and_then(Value::as_str).unwrap_or("GET").parse::<Method>().unwrap_or(Method::GET);
        let resp = match sub_url(url) {
            None => SfError::new("NOT_FOUND", format!("The requested resource does not exist: {url}")).into(),
            Some((version, path, qs)) => {
                let sub_ctx = Ctx { api_version: version, ..ctx.clone() };
                let sp = format!("lf_batch_{i}");
                conn.execute_batch(&format!("SAVEPOINT {sp}"))?;
                let resp = dispatch(
                    conn,
                    &sub_ctx,
                    Req {
                        method,
                        segs: path.split('/').map(url_decode).collect(),
                        query: parse_query(&qs),
                        body: sub.get("richInput").cloned(),
                        batch_size: req.batch_size,
                        st: req.st,
                        cat: req.cat.clone(),
                        headers: HeaderMap::new(),
                    },
                );
                if resp.commits() {
                    conn.execute_batch(&format!("RELEASE {sp}"))?;
                } else {
                    conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp}"))?;
                }
                resp
            }
        };
        let failed = !resp.status.is_success();
        results.push(json!({ "statusCode": resp.status.as_u16(), "result": resp.body }));
        halted = failed && halt_on_error;
    }
    let has_errors = results.iter().any(|r| r["statusCode"].as_u64().unwrap_or(200) >= 400);
    Ok(Resp::ok(json!({ "hasErrors": has_errors, "results": results })))
}

/// `/composite/graph`: several composite requests, each all-or-none on its own.
fn graph(conn: &Connection, ctx: &Ctx, req: &Req) -> SfResult<Resp> {
    let body = body_object(req)?;
    let graphs = body.get("graphs").and_then(Value::as_array).cloned().unwrap_or_default();
    let total: usize =
        graphs.iter().map(|g| g.get("compositeRequest").and_then(Value::as_array).map_or(0, Vec::len)).sum();
    if total > 500 {
        return Err(SfError::new("INVALID_BATCH_REQUEST", "A graph request can contain at most 500 subrequests"));
    }
    let mut out = vec![];
    for (i, g) in graphs.iter().enumerate() {
        let graph_id = g.get("graphId").and_then(Value::as_str).unwrap_or_default();
        let sp = format!("lf_graph_{i}");
        conn.execute_batch(&format!("SAVEPOINT {sp}"))?;
        let sub = Req {
            method: Method::POST,
            segs: vec!["composite".into()],
            query: HashMap::new(),
            body: Some(
                json!({ "allOrNone": true, "compositeRequest": g.get("compositeRequest").cloned().unwrap_or(json!([])) }),
            ),
            batch_size: req.batch_size,
            st: req.st,
            cat: req.cat.clone(),
            headers: HeaderMap::new(),
        };
        let resp = dispatch(conn, ctx, sub);
        let ok = resp.commits();
        if ok {
            conn.execute_batch(&format!("RELEASE {sp}"))?;
        } else {
            conn.execute_batch(&format!("ROLLBACK TO {sp}; RELEASE {sp}"))?;
        }
        out.push(json!({ "graphId": graph_id, "graphResponse": resp.body, "isSuccessful": ok }));
    }
    Ok(Resp::ok(json!({ "graphs": out })))
}

fn halted_error() -> SfError {
    SfError::new(
        "PROCESSING_HALTED",
        "The transaction was rolled back since another operation in the same transaction failed.",
    )
}

/// Replace `@{ref.path}` references with values from earlier subrequests.
fn substitute(s: &str, refs: &HashMap<String, Value>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("@{") {
        out += &rest[..start];
        let Some(end) = rest[start..].find('}') else { break };
        let expr = &rest[start + 2..start + end];
        match lookup(expr, refs) {
            Some(Value::String(v)) => out += &v,
            Some(v) => out += &v.to_string(),
            None => out += &rest[start..start + end + 1],
        }
        rest = &rest[start + end + 1..];
    }
    out + rest
}

fn substitute_json(v: &Value, refs: &HashMap<String, Value>) -> Value {
    match v {
        Value::String(s) => Value::String(substitute(s, refs)),
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute_json(x, refs)).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), substitute_json(x, refs))).collect()),
        other => other.clone(),
    }
}

/// `ref.records[0].Id`
fn lookup(expr: &str, refs: &HashMap<String, Value>) -> Option<Value> {
    let mut parts = expr.split('.');
    let mut cur = refs.get(parts.next()?)?.clone();
    for p in parts {
        let (name, idx) = match p.split_once('[') {
            Some((n, i)) => (n, Some(i.trim_end_matches(']').parse::<usize>().ok()?)),
            None => (p, None),
        };
        cur = cur.get(name)?.clone();
        if let Some(i) = idx {
            cur = cur.get(i)?.clone();
        }
    }
    Some(cur)
}

pub fn parse_query(qs: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(qs.as_bytes()).into_owned().collect()
}

fn url_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// `/limits`, as a 2026 Developer Edition org reports them. Only the API request count moves.
fn limits(used: u64) -> Value {
    const LIMITS: &[(&str, u64)] = &[
        ("AnalyticsExternalDataSizeMB", 40960),
        ("ConcurrentAsyncGetReportInstances", 200),
        ("ConcurrentEinsteinDataInsightsStoryCreation", 5),
        ("ConcurrentEinsteinDiscoveryStoryCreation", 2),
        ("ConcurrentSyncReportRuns", 20),
        ("ContentDistBandwidthLimitMB", 51200),
        ("ContentDistViewLimit", 50000),
        ("ContentPublicationLimit", 2500),
        ("DailyAnalyticsDataflowJobExecutions", 60),
        ("DailyAnalyticsUploadedFilesSizeMB", 51200),
        ("DailyApexCursorLimit", 10000),
        ("DailyApexCursorRowsLimit", 100000000),
        ("DailyApexPCursorLimit", 200000),
        ("DailyApiRequests", API_LIMIT),
        ("DailyAsyncApexExecutions", 250000),
        ("DailyAsyncApexTests", 500),
        ("DailyBulkApiBatches", 15000),
        ("DailyBulkV2QueryFileStorageMB", 976562),
        ("DailyBulkV2QueryJobs", 10000),
        ("DailyDeliveredPlatformEvents", 10000),
        ("DailyDurableGenericStreamingApiEvents", 10000),
        ("DailyDurableStreamingApiEvents", 10000),
        ("DailyEinsteinDataInsightsStoryCreation", 1000),
        ("DailyEinsteinDiscoveryOptimizationJobRuns", 25),
        ("DailyEinsteinDiscoveryPredictAPICalls", 50000),
        ("DailyEinsteinDiscoveryPredictionsByCDC", 500000),
        ("DailyEinsteinDiscoveryStoryCreation", 100),
        ("DailyFunctionsApiCallLimit", 50000),
        ("DailyGenericStreamingApiEvents", 10000),
        ("DailyMetadataRetrievesWithDependencies", 25),
        ("DailyStandardVolumePlatformEvents", 10000),
        ("DailyStreamingApiEvents", 10000),
        ("DailyWorkflowEmails", 5025),
        ("DataStorageMB", 5),
        ("DurableStreamingApiConcurrentClients", 20),
        ("ExternalServicesActiveObjects", 3000),
        ("ExternalServicesActiveOperations", 3000),
        ("ExternalServicesObjectProperties", 400000),
        ("ExternalServicesObjects", 10000),
        ("ExternalServicesOperations", 10000),
        ("ExternalServicesRegistrations", 700),
        ("FileStorageMB", 20),
        ("HourlyAsyncReportRuns", 1200),
        ("HourlyDashboardRefreshes", 200),
        ("HourlyDashboardResults", 5000),
        ("HourlyDashboardStatuses", 999999999),
        ("HourlyElevateAsyncReportRuns", 1200),
        ("HourlyElevateSyncReportRuns", 500),
        ("HourlyLongTermIdMapping", 100000),
        ("HourlyManagedContentPublicRequests", 50000),
        ("HourlyODataCallout", 1000),
        ("HourlyPublishedPlatformEvents", 50000),
        ("HourlyPublishedStandardVolumePlatformEvents", 1000),
        ("HourlyShortTermIdMapping", 100000),
        ("HourlySyncReportRuns", 500),
        ("HourlyTimeBasedWorkflow", 1000),
        ("MassEmail", 10),
        ("MaxContentDocumentsLimit", 30000000),
        ("MonthlyEinsteinDiscoveryStoryCreation", 500),
        ("Package2VersionCreates", 6),
        ("Package2VersionCreatesWithoutValidation", 500),
        ("PermissionSets", 1500),
        ("PlatformEventTriggersWithParallelProcessing", 5),
        ("PrivateConnectOutboundCalloutHourlyLimitMB", 0),
        ("PublishCallbackUsageInApex", 5242880),
        ("ScheduledFlowRunLimit", 250000),
        ("ScheduledPathRunLimit", 250000),
        ("SingleEmail", 15),
        ("StreamingApiConcurrentClients", 20),
    ];
    let mut out = Map::new();
    for (name, max) in LIMITS {
        let remaining = if *name == "DailyApiRequests" { max.saturating_sub(used) } else { *max };
        let mut v = json!({ "Max": max, "Remaining": remaining });
        if *name == "PermissionSets" {
            v["CreateCustom"] = json!({ "Max": 1000, "Remaining": 1000 });
        }
        out.insert(name.to_string(), v);
    }
    Value::Object(out)
}
