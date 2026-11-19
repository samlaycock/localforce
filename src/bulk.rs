//! The Bulk APIs: Bulk API 2.0 under `/services/data/vXX.X/jobs/` and Bulk
//! API 1.0 under `/services/async/XX.X/`.
//!
//! Jobs live in memory (Salesforce keeps them for seven days) and are
//! processed synchronously: a 2.0 ingest job runs when it's marked
//! `UploadComplete`, a 1.0 batch as soon as it's added, and a query when its
//! job (2.0) or batch (1.0) is created. Clients poll as usual and find the
//! work done. Each record is written in its own savepoint, so a failed row
//! doesn't undo the others.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::error::{SfError, SfResult};
use crate::ids;
use crate::rest;
use crate::schema::{Catalog, FieldType, SObject};
use crate::soql;
use crate::soql::parse::{Expr, Query, SelectItem};
use crate::state::{API_LIMIT, AppState};
use crate::store::{self, Ctx, qi};

/// The largest upload accepted: Bulk API 2.0's 150 MB.
pub const MAX_UPLOAD: usize = 150 * 1024 * 1024;

/// Every bulk job, by id.
#[derive(Default)]
pub struct Jobs {
    seq: AtomicU64,
    ingest: Mutex<HashMap<String, IngestJob>>,
    query: Mutex<HashMap<String, QueryJob>>,
    classic: Mutex<HashMap<String, ClassicJob>>,
}

impl Jobs {
    /// A new id with a key prefix: 750 for jobs, 751 for batches, 752 for query results.
    fn new_id(&self, prefix: &str) -> (u64, String) {
        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        (n, ids::make(prefix, n))
    }
}

// ---------------------------------------------------------------------------
// Shared: operations, CSV, records and DML

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Insert,
    Update,
    Upsert,
    Delete,
    HardDelete,
    Query,
    QueryAll,
}

impl Op {
    fn parse(s: &str) -> Option<Op> {
        Some(match s.to_ascii_lowercase().as_str() {
            "insert" => Op::Insert,
            "update" => Op::Update,
            "upsert" => Op::Upsert,
            "delete" => Op::Delete,
            "harddelete" => Op::HardDelete,
            "query" => Op::Query,
            "queryall" => Op::QueryAll,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Op::Insert => "insert",
            Op::Update => "update",
            Op::Upsert => "upsert",
            Op::Delete => "delete",
            Op::HardDelete => "hardDelete",
            Op::Query => "query",
            Op::QueryAll => "queryAll",
        }
    }

    fn is_query(self) -> bool {
        matches!(self, Op::Query | Op::QueryAll)
    }
}

const DELIMITERS: &[(&str, char)] =
    &[("COMMA", ','), ("TAB", '\t'), ("PIPE", '|'), ("SEMICOLON", ';'), ("CARET", '^'), ("BACKQUOTE", '`')];

/// Parse CSV (RFC 4180, with any delimiter and LF or CRLF line endings) into rows.
fn parse_csv(text: &str, delim: char) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = vec![];
    let mut row: Vec<String> = vec![];
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted = false;
    let mut line = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                c => {
                    if c == '\n' {
                        line += 1;
                    }
                    field.push(c);
                }
            }
            continue;
        }
        match c {
            '"' if field.is_empty() && !quoted => {
                in_quotes = true;
                quoted = true;
            }
            c if c == delim => {
                row.push(std::mem::take(&mut field));
                quoted = false;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                quoted = false;
                line += 1;
            }
            '"' => return Err(format!("Invalid quote character in an unquoted field on line {line}")),
            c if quoted => {
                return Err(format!("Unexpected character '{c}' after a closing quote on line {line}"));
            }
            c => field.push(c),
        }
    }
    if in_quotes {
        return Err(format!("Unterminated quoted field on line {line}"));
    }
    if !field.is_empty() || quoted || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty()));
    Ok(rows)
}

/// One CSV line with every value quoted, as Salesforce writes them.
fn csv_line<S: AsRef<str>>(out: &mut String, cells: impl IntoIterator<Item = S>, delim: char, eol: &str) {
    for (i, c) in cells.into_iter().enumerate() {
        if i > 0 {
            out.push(delim);
        }
        out.push('"');
        out.push_str(&c.as_ref().replace('"', "\"\""));
        out.push('"');
    }
    out.push_str(eol);
}

fn eol(line_ending: &str) -> &'static str {
    if line_ending == "CRLF" { "\r\n" } else { "\n" }
}

/// One value of an incoming record.
enum Cell {
    Val(Value),
    Null,
    /// A lookup by a parent's external id: `Account.External_Key__c` in CSV,
    /// `{"Account": {"External_Key__c": "x"}}` in JSON. `None` clears the lookup.
    Rel {
        rel: String,
        field: String,
        value: Option<String>,
    },
}

type InRec = Vec<(String, Cell)>;

/// A CSV row as a record. Empty values are left out (unset); `#N/A` is null.
fn csv_record(header: &[String], row: &[String]) -> InRec {
    let mut rec = vec![];
    for (i, h) in header.iter().enumerate() {
        let v = row.get(i).map(String::as_str).unwrap_or("");
        if v.is_empty() {
            continue;
        }
        let null = v == "#N/A";
        let cell = match h.split_once('.') {
            Some((rel, field)) => {
                Cell::Rel { rel: rel.into(), field: field.into(), value: (!null).then(|| v.to_string()) }
            }
            None if null => Cell::Null,
            None => Cell::Val(Value::String(v.to_string())),
        };
        rec.push((h.clone(), cell));
    }
    rec
}

/// The first column of a CSV header that isn't a field (or `Relationship.Field`) of the object.
fn unknown_column<'h>(obj: &SObject, header: &'h [String]) -> Option<&'h str> {
    header.iter().map(String::as_str).find(|h| match h.split_once('.') {
        Some((rel, _)) => obj.relationship(rel).is_none(),
        None => obj.field(h).is_none(),
    })
}

fn get_ci<'v>(rec: &'v Map<String, Value>, name: &str) -> Option<&'v Value> {
    rec.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

fn take_ci(rec: &mut Map<String, Value>, name: &str) -> Option<Value> {
    let key = rec.keys().find(|k| k.eq_ignore_ascii_case(name))?.clone();
    rec.remove(&key)
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Turn a record's cells into a REST-style body, looking up parents by external id.
fn resolve(conn: &Connection, cat: &Catalog, obj: &SObject, rec: InRec) -> SfResult<Map<String, Value>> {
    let mut out = Map::new();
    for (name, cell) in rec {
        match cell {
            Cell::Val(Value::String(s)) if obj.field(&name).is_some_and(|f| f.ty == FieldType::Boolean) => {
                let b = match s.as_str() {
                    "1" => Value::Bool(true),
                    "0" => Value::Bool(false),
                    _ => Value::String(s),
                };
                out.insert(name, b);
            }
            Cell::Val(v) => {
                out.insert(name, v);
            }
            Cell::Null => {
                out.insert(name, Value::Null);
            }
            Cell::Rel { rel, field, value } => {
                let fk = obj.relationship(&rel).ok_or_else(|| {
                    SfError::new("INVALID_FIELD", format!("No such column '{rel}' on sobject of type {}", obj.name))
                        .with_fields(vec![rel.clone()])
                })?;
                let target = fk.reference_to.as_deref().and_then(|t| cat.object(t)).ok_or_else(|| {
                    SfError::new("INVALID_FIELD", format!("Invalid relationship {rel}")).with_fields(vec![rel.clone()])
                })?;
                let ef = target.field(&field).filter(|f| f.external_id || f.name == "Id").ok_or_else(|| {
                    SfError::new(
                        "INVALID_FIELD",
                        format!(
                            "Field name provided, {field} is not an External ID or indexed field for {}",
                            target.name
                        ),
                    )
                    .with_fields(vec![fk.name.clone()])
                })?;
                let id = match value {
                    None => Value::Null,
                    Some(v) => match store::find_by(conn, target, ef, &v).unwrap_or_default().as_slice() {
                        [id] => Value::String(id.clone()),
                        [] => {
                            return Err(SfError::new(
                                "INVALID_FIELD",
                                format!(
                                    "Foreign key external ID: {v} not found for field {} in entity {}",
                                    ef.name, target.name
                                ),
                            )
                            .with_fields(vec![fk.name.clone()]));
                        }
                        _ => {
                            return Err(SfError::new(
                                "INVALID_FIELD",
                                format!(
                                    "Foreign key external ID: {v} matches more than one record for field {} in entity {}",
                                    ef.name, target.name
                                ),
                            )
                            .with_fields(vec![fk.name.clone()]));
                        }
                    },
                };
                out.insert(fk.name.clone(), id);
            }
        }
    }
    Ok(out)
}

/// Bulk reports a value of the wrong type like the SOAP API does.
fn bulk_error(obj: &SObject, rec: &Map<String, Value>, e: SfError) -> SfError {
    if e.code != "JSON_PARSER_ERROR" {
        return e;
    }
    let Some(f) = e.fields.first().and_then(|n| obj.field(n)) else { return e };
    let v = match get_ci(rec, &f.name) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    };
    SfError::new("INVALID_TYPE_ON_FIELD_IN_RECORD", format!("{}: value not of required type: {v}", f.label))
        .with_fields(vec![f.name.clone()])
}

/// Write one record. Returns its id and whether it was created.
fn apply(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    op: Op,
    ext: Option<&str>,
    mut rec: Map<String, Value>,
) -> SfResult<(String, bool)> {
    let normal = |id: &str| ids::to18(id).unwrap_or_else(|| id.to_string());
    let take_id = |rec: &mut Map<String, Value>| take_ci(rec, "Id").as_ref().and_then(text_of);
    let missing_id = |what: &str| SfError::new("MISSING_ARGUMENT", format!("Id not specified in an {what} call"));
    let insert = |rec: &Map<String, Value>| {
        store::insert(conn, cat, ctx, obj, rec).map(|id| (id, true)).map_err(|e| bulk_error(obj, rec, e))
    };
    let update = |id: &str, rec: &Map<String, Value>| {
        store::update(conn, cat, ctx, obj, id, rec).map(|_| (normal(id), false)).map_err(|e| bulk_error(obj, rec, e))
    };
    match op {
        Op::Insert => insert(&rec),
        Op::Update => {
            let id = take_id(&mut rec).ok_or_else(|| missing_id("update"))?;
            update(&id, &rec)
        }
        Op::Delete | Op::HardDelete => {
            let id = take_id(&mut rec).ok_or_else(|| missing_id("delete"))?;
            store::delete(conn, cat, ctx, obj, &id)?;
            let id = normal(&id);
            if op == Op::HardDelete {
                conn.execute(&format!("DELETE FROM {} WHERE \"Id\" = ?1", qi(&obj.name)), [&id])?;
            }
            Ok((id, false))
        }
        Op::Upsert => {
            let field = ext.unwrap_or("Id");
            if field.eq_ignore_ascii_case("Id") {
                return match take_id(&mut rec) {
                    Some(id) => update(&id, &rec),
                    None => insert(&rec),
                };
            }
            take_id(&mut rec);
            let f = obj.field(field).ok_or_else(SfError::not_found)?;
            let key = get_ci(&rec, field).and_then(text_of).ok_or_else(|| {
                SfError::new("MISSING_ARGUMENT", format!("{field} not specified")).with_fields(vec![f.name.clone()])
            })?;
            match store::find_by(conn, obj, f, &key)?.as_slice() {
                [] => insert(&rec),
                [id] => {
                    take_ci(&mut rec, field);
                    update(id, &rec)
                }
                many => Err(SfError::new(
                    "DUPLICATE_EXTERNAL_ID",
                    format!("{field}: more than one record found for external id field: {}", many.join(", ")),
                )
                .with_fields(vec![f.name.clone()])),
            }
        }
        Op::Query | Op::QueryAll => Err(SfError::new("INVALID_OPERATION", "not a DML operation")),
    }
}

/// What happened to one record: its id and whether it was created, or why it failed.
type Outcome = Result<(String, bool), SfError>;

/// Write records one by one, each in its own savepoint, inside one transaction.
fn process(
    st: &AppState,
    ctx: &Ctx,
    obj: &SObject,
    op: Op,
    ext: Option<&str>,
    recs: Vec<InRec>,
) -> rusqlite::Result<Vec<Outcome>> {
    let cat = st.cat();
    let conn = st.db.lock().unwrap();
    conn.execute_batch("SAVEPOINT lf_bulk")?;
    let mut out = Vec::with_capacity(recs.len());
    for rec in recs {
        conn.execute_batch("SAVEPOINT lf_bulk_row")?;
        let r = resolve(&conn, &cat, obj, rec).and_then(|m| apply(&conn, &cat, ctx, obj, op, ext, m));
        if r.is_ok() {
            conn.execute_batch("RELEASE lf_bulk_row")?;
        } else {
            conn.execute_batch("ROLLBACK TO lf_bulk_row; RELEASE lf_bulk_row")?;
        }
        out.push(r);
    }
    conn.execute_batch("RELEASE lf_bulk")?;
    Ok(out)
}

/// `REQUIRED_FIELD_MISSING:Required fields are missing: [Name]:Name --`
fn error_text(e: &SfError) -> String {
    format!("{}:{}:{} --", e.code, e.message, e.fields.join(","))
}

/// Check an object and external id field for a new ingest job. Returns the
/// object's name and the field's, as the schema spells them.
fn check_job(cat: &Catalog, object: &str, op: Op, ext: Option<&str>) -> Result<(String, Option<String>), String> {
    let obj = cat.object(object).ok_or_else(|| format!("Unable to find object: {object}"))?;
    if op != Op::Upsert {
        return Ok((obj.name.clone(), None));
    }
    let ext = ext.ok_or_else(|| {
        format!("External ID was blank for {}. An External ID must be specified for upsert.", obj.name)
    })?;
    let f = obj
        .field(ext)
        .filter(|f| f.external_id || f.name == "Id")
        .ok_or_else(|| format!("Field name provided, {ext} is not an External ID or indexed field for {}", obj.name))?;
    Ok((obj.name.clone(), Some(f.name.clone())))
}

// ---------------------------------------------------------------------------
// Queries

/// Why a query can't run in bulk, if it can't.
fn unsupported(q: &Query) -> Option<&'static str> {
    let aggregate = |s: &SelectItem| {
        matches!(s, SelectItem::Expr { expr: Expr::Func { name, .. }, .. }
            if matches!(name.to_ascii_uppercase().as_str(), "COUNT" | "COUNT_DISTINCT" | "SUM" | "AVG" | "MIN" | "MAX"))
    };
    if q.select.iter().any(|s| matches!(s, SelectItem::Sub(_))) {
        Some("Aggregate Relationships not supported in Bulk Query")
    } else if !q.group_by.is_empty() || q.select.iter().any(aggregate) {
        Some("Aggregate queries are not supported in Bulk Query")
    } else if q.select.iter().any(|s| matches!(s, SelectItem::Fields { .. })) {
        Some("FIELDS() is not supported in Bulk Query")
    } else if q.offset.is_some() {
        Some("OFFSET is not supported in Bulk Query")
    } else {
        None
    }
}

/// The result columns of a query, as paths spelled like the schema: `Name`, `Account.Name`.
fn query_columns(cat: &Catalog, q: &Query) -> Vec<Vec<String>> {
    let base = cat.object(&q.from);
    q.select
        .iter()
        .filter_map(|s| match s {
            SelectItem::Expr { expr: Expr::Path { parts, .. }, .. } => Some(parts.clone()),
            SelectItem::Expr { expr: Expr::Func { args, .. }, .. } => match args.first() {
                Some(Expr::Path { parts, .. }) => Some(parts.clone()),
                _ => None,
            },
            _ => None,
        })
        .map(|mut parts| {
            let prefixed = parts.len() > 1
                && (q.alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&parts[0]))
                    || base.is_some_and(|o| {
                        o.name.eq_ignore_ascii_case(&parts[0]) && o.relationship(&parts[0]).is_none()
                    }));
            if prefixed {
                parts.remove(0);
            }
            let mut out = vec![];
            let mut obj = base;
            for (i, p) in parts.iter().enumerate() {
                let last = i + 1 == parts.len();
                let name = match obj {
                    Some(o) if last => o.field(p).map(|f| f.name.clone()),
                    Some(o) => o.relationship(p).map(|f| {
                        obj = f.reference_to.as_deref().and_then(|t| cat.object(t));
                        f.relationship_name.clone().unwrap_or_else(|| p.clone())
                    }),
                    None => None,
                };
                if name.is_none() {
                    obj = None;
                }
                out.push(name.unwrap_or_else(|| p.clone()));
            }
            out
        })
        .collect()
}

/// Datetimes in bulk results end in `Z`: `2026-01-31T09:30:00.000Z`.
fn csv_datetime(s: &str) -> String {
    if s.len() == 28 && s.as_bytes()[10] == b'T' && s.ends_with("+0000") {
        format!("{}Z", &s[..23])
    } else {
        s.to_string()
    }
}

/// A query result's value at a path, as CSV text (`None` for null).
fn cell_at(rec: &Value, path: &[String]) -> Option<String> {
    let mut cur = rec;
    for p in path {
        cur = cur.as_object().and_then(|m| get_ci(m, p))?;
    }
    match cur {
        Value::Null => None,
        Value::String(s) => Some(csv_datetime(s)),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// The result of a bulk query: the object, the columns and the records.
struct QueryOutput {
    object: String,
    header: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    records: Vec<Value>,
}

/// Run a bulk query. `Err` is the error Salesforce reports.
fn run_query(st: &AppState, ctx: &Ctx, soql_text: &str, all: bool) -> SfResult<QueryOutput> {
    let q = soql::parse::parse(soql_text)?;
    if let Some(msg) = unsupported(&q) {
        return Err(SfError::new("FUNCTIONALITY_NOT_ENABLED", msg));
    }
    let cat = st.cat();
    let out = {
        let conn = st.db.lock().unwrap();
        soql::run(&conn, &cat, ctx, soql_text, all)?
    };
    let columns = query_columns(&cat, &q);
    let rows = out.records.iter().map(|r| columns.iter().map(|c| cell_at(r, c)).collect()).collect();
    Ok(QueryOutput {
        object: cat.object(&q.from).map(|o| o.name.clone()).unwrap_or(q.from.clone()),
        header: columns.iter().map(|c| c.join(".")).collect(),
        rows,
        records: out.records,
    })
}

fn query_csv(header: &[String], rows: &[Vec<Option<String>>], delim: char, eol: &str) -> String {
    let mut out = String::new();
    csv_line(&mut out, header, delim, eol);
    for r in rows {
        csv_line(&mut out, r.iter().map(|c| c.as_deref().unwrap_or("")), delim, eol);
    }
    out
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Salesforce's query locators are the next offset, base64-encoded without padding.
fn encode_locator(n: usize) -> String {
    let bytes = n.to_string().into_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..=chunk.len() {
            out.push(B64[(v >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn decode_locator(s: &str) -> Option<usize> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut bytes = vec![];
    for c in s.trim_end_matches('=').bytes() {
        bits = bits << 6 | B64.iter().position(|&b| b == c)? as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            bytes.push((bits >> n & 0xff) as u8);
        }
    }
    String::from_utf8(bytes).ok()?.parse().ok()
}

// ---------------------------------------------------------------------------
// Bulk API 2.0

struct IngestJob {
    seq: u64,
    id: String,
    op: Op,
    object: String,
    ext: Option<String>,
    assignment_rule_id: Option<String>,
    created_by: String,
    created: String,
    modstamp: String,
    state: &'static str,
    delim: char,
    delim_name: &'static str,
    line_ending: &'static str,
    api_version: String,
    /// The CSV uploaded, unparsed until the job is processed.
    uploads: Vec<String>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    /// One per row once processed.
    outcomes: Vec<Outcome>,
    error: Option<String>,
    processing_ms: u64,
}

struct QueryJob {
    seq: u64,
    id: String,
    op: Op,
    object: String,
    created_by: String,
    created: String,
    modstamp: String,
    state: &'static str,
    delim: char,
    delim_name: &'static str,
    line_ending: &'static str,
    api_version: String,
    header: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    processing_ms: u64,
}

fn api_version_number(v: &str) -> Value {
    v.parse::<f64>().map(|n| json!(n)).unwrap_or_else(|_| json!(v))
}

impl IngestJob {
    fn info(&self, full: bool) -> Value {
        let mut v = json!({
            "id": self.id,
            "operation": self.op.name(),
            "object": self.object,
            "createdById": self.created_by,
            "createdDate": self.created,
            "systemModstamp": self.modstamp,
            "state": self.state,
        });
        if let Some(e) = &self.ext {
            v["externalIdFieldName"] = json!(e);
        }
        if let Some(a) = &self.assignment_rule_id {
            v["assignmentRuleId"] = json!(a);
        }
        v["concurrencyMode"] = json!("Parallel");
        v["contentType"] = json!("CSV");
        v["apiVersion"] = api_version_number(&self.api_version);
        if !full || self.state == "Open" {
            v["contentUrl"] = json!(format!("services/data/v{}/jobs/ingest/{}/batches", self.api_version, self.id));
        }
        if full {
            v["jobType"] = json!("V2Ingest");
        }
        v["lineEnding"] = json!(self.line_ending);
        v["columnDelimiter"] = json!(self.delim_name);
        if full {
            let failed = self.outcomes.iter().filter(|o| o.is_err()).count();
            v["numberRecordsProcessed"] = json!(self.outcomes.len());
            v["numberRecordsFailed"] = json!(failed);
            v["retries"] = json!(0);
            v["totalProcessingTime"] = json!(self.processing_ms);
            v["apiActiveProcessingTime"] = json!(self.processing_ms);
            v["apexProcessingTime"] = json!(0);
            if let Some(e) = &self.error {
                v["errorMessage"] = json!(e);
            }
        }
        v
    }

    /// Parse the uploaded CSV into one header (the union of every upload's) and rows.
    fn parse_uploads(&self) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
        let mut header: Vec<String> = vec![];
        let mut rows = vec![];
        for up in &self.uploads {
            let mut parsed = parse_csv(up, self.delim)?;
            if parsed.is_empty() {
                continue;
            }
            let h = parsed.remove(0);
            let index: Vec<usize> = h
                .iter()
                .map(|name| {
                    let name = name.trim();
                    header.iter().position(|x| x.eq_ignore_ascii_case(name)).unwrap_or_else(|| {
                        header.push(name.to_string());
                        header.len() - 1
                    })
                })
                .collect();
            for (n, r) in parsed.into_iter().enumerate() {
                if r.len() != h.len() {
                    return Err(format!(
                        "Wrong number of columns in row {}: expected {}, found {}",
                        n + 1,
                        h.len(),
                        r.len()
                    ));
                }
                let mut row = vec![String::new(); index.iter().max().map_or(0, |m| m + 1)];
                for (i, v) in index.iter().zip(r) {
                    row[*i] = v;
                }
                rows.push(row);
            }
        }
        Ok((header, rows))
    }

    /// Process the uploaded data: every row is written or fails on its own.
    fn run(&mut self, st: &AppState) {
        let start = Instant::now();
        self.state = "InProgress";
        let result = (|| -> Result<(), String> {
            let (header, rows) = self.parse_uploads().map_err(|e| format!("InvalidBatch : {e}"))?;
            let cat = st.cat();
            let obj = cat
                .object(&self.object)
                .ok_or_else(|| format!("InvalidJob : Unable to find object: {}", self.object))?;
            if let Some(col) = unknown_column(obj, &header) {
                return Err(format!("InvalidBatch : Field name not found : {col}"));
            }
            let recs = rows.iter().map(|r| csv_record(&header, r)).collect();
            let ctx = st.ctx(&self.created_by, &self.api_version);
            self.outcomes = process(st, &ctx, obj, self.op, self.ext.as_deref(), recs)
                .map_err(|e| format!("InternalServerError : {e}"))?;
            self.header = header;
            self.rows = rows;
            Ok(())
        })();
        self.processing_ms = start.elapsed().as_millis() as u64;
        self.modstamp = now(st);
        match result {
            Ok(()) => self.state = "JobComplete",
            Err(e) => {
                self.state = "Failed";
                self.error = Some(e);
            }
        }
    }

    fn results_csv(&self, kind: &str) -> String {
        let eol = eol(self.line_ending);
        let mut out = String::new();
        let value = |row: &Vec<String>, i: usize| row.get(i).cloned().unwrap_or_default();
        let original = |row: &Vec<String>| (0..self.header.len()).map(|i| value(row, i)).collect::<Vec<_>>();
        match kind {
            "successfulresults" => {
                let mut h = vec!["sf__Id".to_string(), "sf__Created".to_string()];
                h.extend(self.header.iter().cloned());
                csv_line(&mut out, &h, self.delim, eol);
                for (row, o) in self.rows.iter().zip(&self.outcomes) {
                    if let Ok((id, created)) = o {
                        let mut cells = vec![id.clone(), created.to_string()];
                        cells.extend(original(row));
                        csv_line(&mut out, &cells, self.delim, eol);
                    }
                }
            }
            "failedresults" => {
                let mut h = vec!["sf__Id".to_string(), "sf__Error".to_string()];
                h.extend(self.header.iter().cloned());
                csv_line(&mut out, &h, self.delim, eol);
                for (row, o) in self.rows.iter().zip(&self.outcomes) {
                    if let Err(e) = o {
                        let mut cells = vec![String::new(), error_text(e)];
                        cells.extend(original(row));
                        csv_line(&mut out, &cells, self.delim, eol);
                    }
                }
            }
            _ => {
                // Rows never processed: everything uploaded to a job aborted or failed before processing.
                let (header, rows) = if self.outcomes.is_empty() && self.state != "JobComplete" {
                    self.parse_uploads().unwrap_or_default()
                } else {
                    (self.header.clone(), vec![])
                };
                if !header.is_empty() {
                    csv_line(&mut out, &header, self.delim, eol);
                }
                for r in &rows {
                    csv_line(&mut out, (0..header.len()).map(|i| value(r, i)), self.delim, eol);
                }
            }
        }
        out
    }
}

impl QueryJob {
    fn info(&self, full: bool) -> Value {
        let mut v = json!({
            "id": self.id,
            "operation": self.op.name(),
            "object": self.object,
            "createdById": self.created_by,
            "createdDate": self.created,
            "systemModstamp": self.modstamp,
            "state": self.state,
            "concurrencyMode": "Parallel",
            "contentType": "CSV",
            "apiVersion": api_version_number(&self.api_version),
        });
        if full {
            v["jobType"] = json!("V2Query");
        }
        v["lineEnding"] = json!(self.line_ending);
        v["columnDelimiter"] = json!(self.delim_name);
        if full {
            v["numberRecordsProcessed"] = json!(self.rows.len());
            v["retries"] = json!(0);
            v["totalProcessingTime"] = json!(self.processing_ms);
        }
        v
    }
}

fn now(st: &AppState) -> String {
    st.ctx("", "").now_str()
}

fn json_resp(status: StatusCode, v: Value) -> Response {
    (status, axum::Json(v)).into_response()
}

fn csv_resp(body: String) -> Response {
    ([(header::CONTENT_TYPE, "text/csv")], body).into_response()
}

fn json_body(body: &Bytes) -> SfResult<Map<String, Value>> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(SfError::new("JSON_PARSER_ERROR", "The request body is either invalid or incomplete")),
        Err(e) => Err(SfError::new("JSON_PARSER_ERROR", format!("Unexpected character: {e}"))),
    }
}

fn str_field(b: &Map<String, Value>, k: &str) -> Option<String> {
    b.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

fn enum_error(kind: &str, value: &str) -> SfError {
    SfError::new(
        "JSON_PARSER_ERROR",
        format!(
            "Cannot deserialize instance of {kind} from VALUE_STRING value {value} or request may be missing a required field"
        ),
    )
}

/// `columnDelimiter` and `lineEnding` of a new job.
fn csv_options(b: &Map<String, Value>) -> SfResult<(char, &'static str, &'static str)> {
    let (delim_name, delim) = match str_field(b, "columnDelimiter") {
        None => DELIMITERS[0],
        Some(d) => *DELIMITERS.iter().find(|(n, _)| *n == d).ok_or_else(|| enum_error("ColumnDelimiterEnum", &d))?,
    };
    let line_ending = match str_field(b, "lineEnding").as_deref() {
        None | Some("LF") => "LF",
        Some("CRLF") => "CRLF",
        Some(other) => return Err(enum_error("LineEndingEnum", other)),
    };
    if let Some(ct) = str_field(b, "contentType").filter(|c| c != "CSV") {
        return Err(enum_error("ContentType", &ct));
    }
    Ok((delim, delim_name, line_ending))
}

fn job_not_found() -> SfError {
    SfError::new("NOT_FOUND", "The requested resource does not exist")
}

fn invalid_state(msg: impl Into<String>) -> SfError {
    SfError::new("INVALIDJOBSTATE", msg)
}

/// Lists of jobs page by 1,000, like Salesforce.
fn job_list(records: Vec<Value>, offset: usize, next_base: &str) -> Value {
    const PAGE: usize = 1000;
    let end = (offset + PAGE).min(records.len());
    let page: Vec<Value> = records[offset.min(end)..end].to_vec();
    let done = end >= records.len();
    json!({
        "done": done,
        "records": page,
        "nextRecordsUrl": if done { Value::Null } else { json!(format!("{next_base}?queryLocator={end}")) },
    })
}

/// `/services/data/vXX.X/jobs/{*rest}`
pub async fn v2(
    State(st): State<AppState>,
    Path((version, rest)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(v) = version.strip_prefix('v').filter(|v| rest::supported_version(v)) else {
        return SfError::not_found().into_response();
    };
    let Some(user) = crate::routes::bearer(&headers).and_then(|t| st.user_for_token(&t)) else {
        return SfError::new("INVALID_SESSION_ID", "Session expired or invalid").into_response();
    };
    let used = st.count_call();
    let ctx = st.ctx(&user, v);
    let params = rest::parse_query(&query.unwrap_or_default());
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = v2_route(&st, &ctx, &method, &segs, &params, &body).unwrap_or_else(IntoResponse::into_response);
    out.headers_mut()
        .insert("sforce-limit-info", HeaderValue::from_str(&format!("api-usage={used}/{API_LIMIT}")).unwrap());
    out
}

fn v2_route(
    st: &AppState,
    ctx: &Ctx,
    m: &Method,
    segs: &[&str],
    params: &HashMap<String, String>,
    body: &Bytes,
) -> SfResult<Response> {
    let not_allowed = |allowed: &str| {
        SfError::new("METHOD_NOT_ALLOWED", format!("HTTP Method '{m}' not allowed. Allowed are {allowed}"))
    };
    match segs {
        ["ingest"] => match *m {
            Method::POST => create_ingest(st, ctx, body),
            Method::GET => {
                let jobs = st.bulk.ingest.lock().unwrap();
                // Every job here is a parallel V2Ingest job.
                let matches = params.get("jobType").is_none_or(|t| t == "V2Ingest")
                    && params.get("concurrencyMode").is_none_or(|c| c == "Parallel");
                let mut list: Vec<&IngestJob> = jobs.values().filter(|_| matches).collect();
                list.sort_by_key(|j| j.seq);
                let records = list
                    .iter()
                    .map(|j| {
                        let mut v = j.info(false);
                        v["jobType"] = json!("V2Ingest");
                        v
                    })
                    .collect();
                let offset = params.get("queryLocator").and_then(|l| l.parse().ok()).unwrap_or(0);
                let base = format!("/services/data/v{}/jobs/ingest", ctx.api_version);
                Ok(json_resp(StatusCode::OK, job_list(records, offset, &base)))
            }
            _ => Err(not_allowed("GET,POST")),
        },
        ["ingest", id] => {
            let mut jobs = st.bulk.ingest.lock().unwrap();
            let job = jobs.get_mut(*id).ok_or_else(job_not_found)?;
            match *m {
                Method::GET => Ok(json_resp(StatusCode::OK, job.info(true))),
                Method::PATCH => {
                    let b = json_body(body)?;
                    let state = str_field(&b, "state").unwrap_or_default();
                    match (state.as_str(), job.state) {
                        ("UploadComplete", "Open") => {
                            job.state = "UploadComplete";
                            let resp = job.info(false);
                            job.run(st);
                            Ok(json_resp(StatusCode::OK, resp))
                        }
                        ("Aborted", "Open" | "UploadComplete" | "InProgress") => {
                            job.state = "Aborted";
                            job.modstamp = now(st);
                            Ok(json_resp(StatusCode::OK, job.info(false)))
                        }
                        ("UploadComplete" | "Aborted", from) => {
                            Err(invalid_state(format!("Unable to change the job's state from {from} to {state}")))
                        }
                        (other, _) => Err(enum_error("JobStateEnum", other)),
                    }
                }
                Method::DELETE => {
                    if job.state == "Open" {
                        return Err(invalid_state("A job must be closed or aborted before it can be deleted"));
                    }
                    jobs.remove(*id);
                    Ok(StatusCode::NO_CONTENT.into_response())
                }
                _ => Err(not_allowed("GET,PATCH,DELETE")),
            }
        }
        ["ingest", id, "batches"] => {
            if m != Method::PUT {
                return Err(not_allowed("PUT"));
            }
            let mut jobs = st.bulk.ingest.lock().unwrap();
            let job = jobs.get_mut(*id).ok_or_else(job_not_found)?;
            if job.state != "Open" {
                return Err(invalid_state(format!(
                    "The job's state is {}: data can only be uploaded to an Open job",
                    job.state
                )));
            }
            job.uploads.push(String::from_utf8_lossy(body).into_owned());
            Ok(StatusCode::CREATED.into_response())
        }
        ["ingest", id, kind]
            if matches!(
                kind.to_ascii_lowercase().as_str(),
                "successfulresults" | "failedresults" | "unprocessedrecords"
            ) =>
        {
            if m != Method::GET {
                return Err(not_allowed("GET"));
            }
            let jobs = st.bulk.ingest.lock().unwrap();
            let job = jobs.get(*id).ok_or_else(job_not_found)?;
            if !matches!(job.state, "JobComplete" | "Failed" | "Aborted") {
                return Err(invalid_state(format!(
                    "The job's state is {}: results are available once it's complete",
                    job.state
                )));
            }
            Ok(csv_resp(job.results_csv(&kind.to_ascii_lowercase())))
        }
        ["query"] => match *m {
            Method::POST => create_query(st, ctx, body),
            Method::GET => {
                let jobs = st.bulk.query.lock().unwrap();
                let matches = params.get("jobType").is_none_or(|t| t == "V2Query");
                let mut list: Vec<&QueryJob> = jobs.values().filter(|_| matches).collect();
                list.sort_by_key(|j| j.seq);
                let records = list
                    .iter()
                    .map(|j| {
                        let mut v = j.info(false);
                        v["jobType"] = json!("V2Query");
                        v
                    })
                    .collect();
                let offset = params.get("queryLocator").and_then(|l| l.parse().ok()).unwrap_or(0);
                let base = format!("/services/data/v{}/jobs/query", ctx.api_version);
                Ok(json_resp(StatusCode::OK, job_list(records, offset, &base)))
            }
            _ => Err(not_allowed("GET,POST")),
        },
        ["query", id] => {
            let mut jobs = st.bulk.query.lock().unwrap();
            let job = jobs.get_mut(*id).ok_or_else(job_not_found)?;
            match *m {
                Method::GET => Ok(json_resp(StatusCode::OK, job.info(true))),
                Method::PATCH => {
                    let b = json_body(body)?;
                    let state = str_field(&b, "state").unwrap_or_default();
                    match (state.as_str(), job.state) {
                        ("Aborted", "UploadComplete" | "InProgress") => {
                            job.state = "Aborted";
                            job.modstamp = now(st);
                            Ok(json_resp(StatusCode::OK, job.info(false)))
                        }
                        ("Aborted", from) => {
                            Err(invalid_state(format!("Unable to change the job's state from {from} to Aborted")))
                        }
                        (other, _) => Err(enum_error("JobStateEnum", other)),
                    }
                }
                Method::DELETE => {
                    jobs.remove(*id);
                    Ok(StatusCode::NO_CONTENT.into_response())
                }
                _ => Err(not_allowed("GET,PATCH,DELETE")),
            }
        }
        ["query", id, "results"] => {
            if m != Method::GET {
                return Err(not_allowed("GET"));
            }
            let jobs = st.bulk.query.lock().unwrap();
            let job = jobs.get(*id).ok_or_else(job_not_found)?;
            if job.state != "JobComplete" {
                return Err(invalid_state(format!(
                    "The job's state is {}: results are available once it's complete",
                    job.state
                )));
            }
            let offset = match params.get("locator").map(String::as_str).filter(|l| !l.is_empty() && *l != "null") {
                None => 0,
                Some(l) => decode_locator(l)
                    .filter(|n| *n <= job.rows.len())
                    .ok_or_else(|| SfError::new("INVALID_QUERY_LOCATOR", format!("Invalid locator: {l}")))?,
            };
            // Without maxRecords (or with a bad one), pages are as big as simple-salesforce's default.
            let max =
                params.get("maxRecords").and_then(|n| n.parse::<usize>().ok()).filter(|n| *n > 0).unwrap_or(50_000);
            let end = (offset + max).min(job.rows.len());
            let body = query_csv(&job.header, &job.rows[offset..end], job.delim, eol(job.line_ending));
            let locator = if end < job.rows.len() { encode_locator(end) } else { "null".into() };
            let mut resp = csv_resp(body);
            let h = resp.headers_mut();
            h.insert("sforce-locator", HeaderValue::from_str(&locator).unwrap());
            h.insert("sforce-numberofrecords", HeaderValue::from(end - offset));
            Ok(resp)
        }
        _ => Err(SfError::not_found()),
    }
}

fn create_ingest(st: &AppState, ctx: &Ctx, body: &Bytes) -> SfResult<Response> {
    let b = json_body(body)?;
    let op_name = str_field(&b, "operation")
        .ok_or_else(|| SfError::new("INVALIDJOB", "InvalidJob : The operation field is required"))?;
    let op = Op::parse(&op_name).filter(|o| !o.is_query()).ok_or_else(|| enum_error("OperationEnum", &op_name))?;
    let object = str_field(&b, "object")
        .ok_or_else(|| SfError::new("INVALIDJOB", "InvalidJob : The object field is required"))?;
    let (delim, delim_name, line_ending) = csv_options(&b)?;
    let (object, ext) = check_job(&st.cat(), &object, op, str_field(&b, "externalIdFieldName").as_deref())
        .map_err(|e| SfError::new("INVALIDJOB", e))?;
    let (seq, id) = st.bulk.new_id("750");
    let created = ctx.now_str();
    let job = IngestJob {
        seq,
        id: id.clone(),
        op,
        object,
        ext,
        assignment_rule_id: str_field(&b, "assignmentRuleId"),
        created_by: ctx.user_id.clone(),
        modstamp: created.clone(),
        created,
        state: "Open",
        delim,
        delim_name,
        line_ending,
        api_version: ctx.api_version.clone(),
        uploads: vec![],
        header: vec![],
        rows: vec![],
        outcomes: vec![],
        error: None,
        processing_ms: 0,
    };
    let info = job.info(false);
    st.bulk.ingest.lock().unwrap().insert(id, job);
    Ok(json_resp(StatusCode::OK, info))
}

fn create_query(st: &AppState, ctx: &Ctx, body: &Bytes) -> SfResult<Response> {
    let b = json_body(body)?;
    let op_name = str_field(&b, "operation").unwrap_or_else(|| "query".into());
    let op = Op::parse(&op_name).filter(|o| o.is_query()).ok_or_else(|| enum_error("OperationEnum", &op_name))?;
    let soql_text =
        str_field(&b, "query").ok_or_else(|| SfError::new("INVALIDJOB", "InvalidJob : The query field is required"))?;
    let (delim, delim_name, line_ending) = csv_options(&b)?;
    let start = Instant::now();
    let out = run_query(st, ctx, &soql_text, op == Op::QueryAll)?;
    let (seq, id) = st.bulk.new_id("750");
    let created = ctx.now_str();
    let mut job = QueryJob {
        seq,
        id: id.clone(),
        op,
        object: out.object,
        created_by: ctx.user_id.clone(),
        modstamp: created.clone(),
        created,
        // Salesforce answers before it runs the query.
        state: "UploadComplete",
        delim,
        delim_name,
        line_ending,
        api_version: ctx.api_version.clone(),
        header: out.header,
        rows: out.rows,
        processing_ms: start.elapsed().as_millis() as u64,
    };
    let info = job.info(false);
    job.state = "JobComplete";
    st.bulk.query.lock().unwrap().insert(id, job);
    Ok(json_resp(StatusCode::OK, info))
}

// ---------------------------------------------------------------------------
// Bulk API 1.0

const NS: &str = "http://www.force.com/2009/06/asyncapi/dataload";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ct {
    Csv,
    Xml,
    Json,
}

impl Ct {
    fn name(self) -> &'static str {
        match self {
            Ct::Csv => "CSV",
            Ct::Xml => "XML",
            Ct::Json => "JSON",
        }
    }
    fn mime(self) -> &'static str {
        match self {
            Ct::Csv => "text/csv",
            Ct::Xml => "application/xml",
            Ct::Json => "application/json",
        }
    }
}

struct ClassicJob {
    id: String,
    op: Op,
    object: String,
    ext: Option<String>,
    assignment_rule_id: Option<String>,
    created_by: String,
    created: chrono::DateTime<chrono::Utc>,
    modstamp: chrono::DateTime<chrono::Utc>,
    state: &'static str,
    serial: bool,
    ct: Ct,
    api_version: String,
    batches: Vec<Batch>,
}

struct Batch {
    id: String,
    job_id: String,
    state: &'static str,
    state_message: Option<String>,
    created: chrono::DateTime<chrono::Utc>,
    modstamp: chrono::DateTime<chrono::Utc>,
    request: String,
    outcomes: Vec<Outcome>,
    query: Option<(String, QueryOutput)>,
    processed: usize,
    failed: usize,
    ms: u64,
}

/// An error in Bulk API 1.0's format.
struct AsyncError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn async_err(code: &'static str, message: impl Into<String>) -> AsyncError {
    AsyncError { status: StatusCode::BAD_REQUEST, code, message: message.into() }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_resp(status: StatusCode, root: &str, inner: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml")],
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><{root} xmlns="{NS}">{inner}</{root}>"#),
    )
        .into_response()
}

impl AsyncError {
    fn render(self, json: bool) -> Response {
        if json {
            json_resp(self.status, json!({ "exceptionCode": self.code, "exceptionMessage": self.message }))
        } else {
            xml_resp(
                self.status,
                "error",
                &format!(
                    "<exceptionCode>{}</exceptionCode><exceptionMessage>{}</exceptionMessage>",
                    self.code,
                    xml_escape(&self.message)
                ),
            )
        }
    }
}

/// `2026-01-31T09:30:00.000Z` (XML) or `2026-01-31T09:30:00.000+0000` (JSON).
fn async_date(t: chrono::DateTime<chrono::Utc>, json: bool) -> String {
    if json { store::format_datetime(t) } else { t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string() }
}

/// Write `(name, value)` pairs as XML elements; `None` values are left out.
fn xml_fields(fields: &[(&str, Option<String>)]) -> String {
    fields.iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("<{k}>{}</{k}>", xml_escape(v)))).collect()
}

impl ClassicJob {
    fn counts(&self) -> (usize, usize, usize, usize) {
        let completed = self.batches.iter().filter(|b| b.state == "Completed").count();
        let failed = self.batches.iter().filter(|b| b.state == "Failed").count();
        let processed = self.batches.iter().map(|b| b.processed).sum();
        let records_failed = self.batches.iter().map(|b| b.failed).sum();
        (completed, failed, processed, records_failed)
    }

    fn render(&self, status: StatusCode, json: bool) -> Response {
        let (completed, failed, processed, records_failed) = self.counts();
        let ms: u64 = self.batches.iter().map(|b| b.ms).sum();
        let concurrency = if self.serial { "Serial" } else { "Parallel" };
        if json {
            // Salesforce's JSON job info lists its fields alphabetically.
            return json_resp(
                status,
                json!({
                    "apexProcessingTime": 0,
                    "apiActiveProcessingTime": ms,
                    "apiVersion": api_version_number(&self.api_version),
                    "assignmentRuleId": self.assignment_rule_id,
                    "concurrencyMode": concurrency,
                    "contentType": self.ct.name(),
                    "createdById": self.created_by,
                    "createdDate": async_date(self.created, true),
                    "externalIdFieldName": self.ext,
                    "fastPathEnabled": false,
                    "id": self.id,
                    "numberBatchesCompleted": completed,
                    "numberBatchesFailed": failed,
                    "numberBatchesInProgress": 0,
                    "numberBatchesQueued": 0,
                    "numberBatchesTotal": self.batches.len(),
                    "numberRecordsFailed": records_failed,
                    "numberRecordsProcessed": processed,
                    "numberRetries": 0,
                    "object": self.object,
                    "operation": self.op.name(),
                    "state": self.state,
                    "systemModstamp": async_date(self.modstamp, true),
                    "totalProcessingTime": ms,
                }),
            );
        }
        let n = |x: usize| Some(x.to_string());
        let inner = xml_fields(&[
            ("id", Some(self.id.clone())),
            ("operation", Some(self.op.name().into())),
            ("object", Some(self.object.clone())),
            ("createdById", Some(self.created_by.clone())),
            ("createdDate", Some(async_date(self.created, false))),
            ("systemModstamp", Some(async_date(self.modstamp, false))),
            ("state", Some(self.state.into())),
            ("externalIdFieldName", self.ext.clone()),
            ("assignmentRuleId", self.assignment_rule_id.clone()),
            ("concurrencyMode", Some(concurrency.into())),
            ("contentType", Some(self.ct.name().into())),
            ("numberBatchesQueued", n(0)),
            ("numberBatchesInProgress", n(0)),
            ("numberBatchesCompleted", n(completed)),
            ("numberBatchesFailed", n(failed)),
            ("numberBatchesTotal", n(self.batches.len())),
            ("numberRecordsProcessed", n(processed)),
            ("numberRetries", n(0)),
            ("apiVersion", Some(self.api_version.clone())),
            ("numberRecordsFailed", n(records_failed)),
            ("totalProcessingTime", Some(ms.to_string())),
            ("apiActiveProcessingTime", Some(ms.to_string())),
            ("apexProcessingTime", n(0)),
        ]);
        xml_resp(status, "jobInfo", &inner)
    }
}

impl Batch {
    fn json(&self) -> Value {
        json!({
            "apexProcessingTime": 0,
            "apiActiveProcessingTime": self.ms,
            "createdDate": async_date(self.created, true),
            "id": self.id,
            "jobId": self.job_id,
            "numberRecordsFailed": self.failed,
            "numberRecordsProcessed": self.processed,
            "state": self.state,
            "stateMessage": self.state_message,
            "systemModstamp": async_date(self.modstamp, true),
            "totalProcessingTime": self.ms,
        })
    }

    fn xml(&self) -> String {
        xml_fields(&[
            ("id", Some(self.id.clone())),
            ("jobId", Some(self.job_id.clone())),
            ("state", Some(self.state.into())),
            ("stateMessage", self.state_message.clone()),
            ("createdDate", Some(async_date(self.created, false))),
            ("systemModstamp", Some(async_date(self.modstamp, false))),
            ("numberRecordsProcessed", Some(self.processed.to_string())),
            ("numberRecordsFailed", Some(self.failed.to_string())),
            ("totalProcessingTime", Some(self.ms.to_string())),
            ("apiActiveProcessingTime", Some(self.ms.to_string())),
            ("apexProcessingTime", Some("0".into())),
        ])
    }

    fn render(&self, status: StatusCode, json: bool) -> Response {
        if json { json_resp(status, self.json()) } else { xml_resp(status, "batchInfo", &self.xml()) }
    }
}

/// The session from `X-SFDC-Session`, or a bearer token.
fn async_session(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-sfdc-session")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim().to_string())
        .or_else(|| crate::routes::bearer(headers))
}

/// `/services/async/XX.X/{*rest}`
pub async fn v1(
    State(st): State<AppState>,
    Path((version, rest)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json_req = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|c| c.to_ascii_lowercase().contains("json"));
    if !rest::supported_version(&version) {
        return AsyncError {
            status: StatusCode::NOT_FOUND,
            code: "InvalidUrl",
            message: format!("Unsupported API version: {version}"),
        }
        .render(json_req);
    }
    let Some(user) = async_session(&headers).and_then(|t| st.user_for_token(&t)) else {
        return async_err("InvalidSessionId", "Invalid session id").render(json_req);
    };
    st.count_call();
    let ctx = st.ctx(&user, &version);
    let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    match v1_route(&st, &ctx, &method, &segs, json_req, &body) {
        Ok(r) => r,
        Err((e, json)) => e.render(json),
    }
}

type V1Result = Result<Response, (AsyncError, bool)>;

fn v1_route(st: &AppState, ctx: &Ctx, m: &Method, segs: &[&str], json_req: bool, body: &Bytes) -> V1Result {
    let bad_method = |json| {
        (
            AsyncError {
                status: StatusCode::METHOD_NOT_ALLOWED,
                code: "InvalidUrl",
                message: format!("HTTP method '{m}' not allowed"),
            },
            json,
        )
    };
    if let ["job"] = segs {
        return if m == Method::POST { create_classic(st, ctx, json_req, body) } else { Err(bad_method(json_req)) };
    }
    let Some(job_id) = (match segs {
        ["job", id, ..] => Some(*id),
        _ => None,
    }) else {
        return Err((
            AsyncError { status: StatusCode::NOT_FOUND, code: "InvalidUrl", message: "Unknown URL".into() },
            json_req,
        ));
    };
    let mut jobs = st.bulk.classic.lock().unwrap();
    let Some(job) = jobs.get_mut(job_id) else {
        return Err((async_err("InvalidJob", format!("Invalid job id: {job_id}")), json_req));
    };
    let json = json_req || job.ct == Ct::Json;
    let err = |code, msg: String| Err((async_err(code, msg), json));
    match (segs, m.clone()) {
        ([_, _], Method::GET) => Ok(job.render(StatusCode::OK, json)),
        ([_, _], Method::POST) => {
            let state = if json {
                serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v.get("state").and_then(Value::as_str).map(String::from))
            } else {
                xml_text(&String::from_utf8_lossy(body), "state")
            };
            match (state.as_deref(), job.state) {
                (Some("Closed"), "Open") => job.state = "Closed",
                (Some("Aborted"), "Open" | "Closed") => job.state = "Aborted",
                (Some(s @ ("Closed" | "Aborted")), from) => {
                    return err("InvalidJobState", format!("Unable to change the job's state from {from} to {s}"));
                }
                (s, _) => return err("InvalidJob", format!("Invalid state: {}", s.unwrap_or(""))),
            }
            job.modstamp = st.ctx("", "").now;
            Ok(job.render(StatusCode::OK, json))
        }
        ([_, _, "batch"], Method::POST) => {
            if job.state != "Open" {
                return err("InvalidJobState", format!("Job {} is not open: its state is {}", job.id, job.state));
            }
            let text = String::from_utf8_lossy(body);
            let empty = text.trim().is_empty()
                || (job.ct == Ct::Json && serde_json::from_str::<Value>(&text).is_ok_and(|v| v == json!([])));
            if empty {
                return err("ClientInputError", "Unable to find any data to create batch".into());
            }
            let batch = run_batch(st, ctx, job, body);
            let resp = batch.render(StatusCode::CREATED, json);
            job.batches.push(batch);
            Ok(resp)
        }
        ([_, _, "batch"], Method::GET) => {
            if json {
                let list: Vec<Value> = job.batches.iter().map(Batch::json).collect();
                Ok(json_resp(StatusCode::OK, json!({ "batchInfo": list })))
            } else {
                let inner: String = job.batches.iter().map(|b| format!("<batchInfo>{}</batchInfo>", b.xml())).collect();
                Ok(xml_resp(StatusCode::OK, "batchInfoList", &inner))
            }
        }
        ([_, _, "batch", bid, rest @ ..], Method::GET) => {
            let Some(batch) = job.batches.iter().find(|b| b.id == *bid || ids::to18(bid).as_deref() == Some(&b.id))
            else {
                return err("InvalidBatch", format!("Invalid batch id: {bid}"));
            };
            match rest {
                [] => Ok(batch.render(StatusCode::OK, json)),
                ["request"] => Ok(([(header::CONTENT_TYPE, request_mime(job))], batch.request.clone()).into_response()),
                ["result", more @ ..] => {
                    if batch.state != "Completed" {
                        return err(
                            "InvalidBatch",
                            format!(
                                "Batch {} is {}: {}",
                                batch.id,
                                batch.state,
                                batch.state_message.as_deref().unwrap_or("results are not available")
                            ),
                        );
                    }
                    match (more, &batch.query) {
                        ([], None) => Ok(dml_results(job.ct, json, &batch.outcomes)),
                        ([], Some((rid, _))) => {
                            if json {
                                Ok(json_resp(StatusCode::OK, json!([rid])))
                            } else {
                                Ok(xml_resp(StatusCode::OK, "result-list", &format!("<result>{rid}</result>")))
                            }
                        }
                        ([rid], Some((id, out))) if *rid == id => Ok(query_results(st, job.ct, out)),
                        _ => err("InvalidBatch", "Invalid result id".to_string()),
                    }
                }
                _ => Err((
                    AsyncError { status: StatusCode::NOT_FOUND, code: "InvalidUrl", message: "Unknown URL".into() },
                    json,
                )),
            }
        }
        _ => Err(bad_method(json)),
    }
}

fn request_mime(job: &ClassicJob) -> &'static str {
    if job.op.is_query() { "text/plain" } else { job.ct.mime() }
}

/// The text of the first element named `tag`.
fn xml_text(body: &str, tag: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let node = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == tag)?;
    Some(node.text().unwrap_or_default().trim().to_string())
}

fn create_classic(st: &AppState, ctx: &Ctx, json_req: bool, body: &Bytes) -> V1Result {
    let text = String::from_utf8_lossy(body);
    let fail = |code, msg: String| Err((async_err(code, msg), json_req));
    // The job's fields, as text.
    let fields: HashMap<String, String> = if json_req {
        let Ok(Value::Object(b)) = serde_json::from_str::<Value>(&text) else {
            return fail("ClientInputError", "Unable to parse the job: invalid JSON".into());
        };
        b.into_iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) if !s.trim().is_empty() => Some((k, s.trim().to_string())),
                Value::Number(n) => Some((k, n.to_string())),
                _ => None,
            })
            .collect()
    } else {
        let Ok(doc) = roxmltree::Document::parse(text.trim()) else {
            return fail("InvalidXML", "Unable to parse the job: invalid XML".into());
        };
        doc.root_element()
            .children()
            .filter(|n| n.is_element())
            .map(|n| (n.tag_name().name().to_string(), n.text().unwrap_or_default().trim().to_string()))
            .filter(|(_, v)| !v.is_empty())
            .collect()
    };
    let get = |k: &str| fields.get(k).cloned();
    let op_name = get("operation").unwrap_or_default();
    let Some(op) = Op::parse(&op_name) else {
        return fail("InvalidJob", format!("Invalid job operation: {op_name}"));
    };
    let object = get("object").unwrap_or_default();
    let ct = match get("contentType").as_deref() {
        None if json_req => Ct::Json,
        None | Some("XML") => Ct::Xml,
        Some("CSV") => Ct::Csv,
        Some("JSON") => Ct::Json,
        Some(other) => return fail("InvalidJob", format!("Unsupported content type: {other}")),
    };
    let (object, ext) = if op.is_query() {
        match st.cat().object(&object) {
            Some(o) => (o.name.clone(), None),
            None => return fail("InvalidJob", format!("Unable to find object: {object}")),
        }
    } else {
        match check_job(&st.cat(), &object, op, get("externalIdFieldName").as_deref()) {
            Ok(x) => x,
            Err(e) => return fail("InvalidJob", e),
        }
    };
    let serial = matches!(get("concurrencyMode").as_deref(), Some("Serial" | "1"));
    let (_, id) = st.bulk.new_id("750");
    let job = ClassicJob {
        id: id.clone(),
        op,
        object,
        ext,
        assignment_rule_id: get("assignmentRuleId"),
        created_by: ctx.user_id.clone(),
        created: ctx.now,
        modstamp: ctx.now,
        state: "Open",
        serial,
        ct,
        api_version: ctx.api_version.clone(),
        batches: vec![],
    };
    let resp = job.render(StatusCode::CREATED, json_req || ct == Ct::Json);
    st.bulk.classic.lock().unwrap().insert(id, job);
    Ok(resp)
}

/// Records from a JSON batch: `[{"Name": "x", "Account": {"External_Key__c": "k"}}]`.
fn json_records(text: &str) -> Result<Vec<InRec>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("Unable to parse the batch: {e}"))?;
    let Value::Array(items) = v else { return Err("A JSON batch must be an array of records".into()) };
    items
        .into_iter()
        .map(|item| {
            let Value::Object(m) = item else { return Err("A JSON batch must be an array of records".to_string()) };
            Ok(m.into_iter()
                .filter(|(k, _)| k != "attributes")
                .map(|(k, v)| {
                    let cell = match v {
                        Value::Null => Cell::Null,
                        Value::Object(parent) => {
                            let (field, value) = parent
                                .into_iter()
                                .find(|(k, _)| k != "attributes")
                                .unwrap_or_else(|| ("Id".into(), Value::Null));
                            Cell::Rel { rel: k.clone(), field, value: text_of(&value) }
                        }
                        other => Cell::Val(other),
                    };
                    (k, cell)
                })
                .collect())
        })
        .collect()
}

/// Records from an XML batch: `<sObjects><sObject><Name>x</Name></sObject></sObjects>`.
fn xml_records(text: &str) -> Result<Vec<InRec>, String> {
    let doc = roxmltree::Document::parse(text.trim()).map_err(|e| format!("Unable to parse the batch: {e}"))?;
    let xsi = "http://www.w3.org/2001/XMLSchema-instance";
    Ok(doc
        .root_element()
        .children()
        .filter(|n| n.is_element())
        .map(|rec| {
            rec.children()
                .filter(|n| n.is_element())
                .map(|f| {
                    let name = f.tag_name().name().to_string();
                    let parent = f.children().find(|c| c.is_element() && c.tag_name().name() != "type");
                    let cell = if f.attribute((xsi, "nil")) == Some("true") {
                        Cell::Null
                    } else if let Some(p) = parent {
                        let nil = p.attribute((xsi, "nil")) == Some("true");
                        Cell::Rel {
                            rel: name.clone(),
                            field: p.tag_name().name().to_string(),
                            value: (!nil).then(|| p.text().unwrap_or_default().to_string()),
                        }
                    } else {
                        Cell::Val(Value::String(f.text().unwrap_or_default().to_string()))
                    };
                    (name, cell)
                })
                .collect()
        })
        .collect())
}

/// Run one batch: the records of a DML job, or a query job's SOQL.
fn run_batch(st: &AppState, ctx: &Ctx, job: &ClassicJob, body: &Bytes) -> Batch {
    let start = Instant::now();
    let request = String::from_utf8_lossy(body).into_owned();
    let (_, id) = st.bulk.new_id("751");
    let mut batch = Batch {
        id,
        job_id: job.id.clone(),
        state: "Completed",
        state_message: None,
        created: ctx.now,
        modstamp: ctx.now,
        request,
        outcomes: vec![],
        query: None,
        processed: 0,
        failed: 0,
        ms: 0,
    };
    let result: Result<(), String> = (|| {
        if job.op.is_query() {
            let out = run_query(st, ctx, batch.request.trim(), job.op == Op::QueryAll)
                .map_err(|e| format!("InvalidBatch : Failed to process query: {}: {}", e.code, e.message))?;
            batch.processed = out.rows.len();
            let (_, rid) = st.bulk.new_id("752");
            batch.query = Some((rid, out));
            return Ok(());
        }
        let cat = st.cat();
        let obj =
            cat.object(&job.object).ok_or_else(|| format!("InvalidJob : Unable to find object: {}", job.object))?;
        let recs = match job.ct {
            Ct::Csv => {
                let mut rows = parse_csv(&batch.request, ',').map_err(|e| format!("InvalidBatch : {e}"))?;
                if rows.is_empty() {
                    return Err("InvalidBatch : The batch is empty".into());
                }
                let header: Vec<String> = rows.remove(0).into_iter().map(|h| h.trim().to_string()).collect();
                if let Some(col) = unknown_column(obj, &header) {
                    return Err(format!("InvalidBatch : Field name not found : {col}"));
                }
                if let Some(n) = rows.iter().position(|r| r.len() != header.len()) {
                    return Err(format!("InvalidBatch : Wrong number of columns in row {}", n + 1));
                }
                rows.iter().map(|r| csv_record(&header, r)).collect()
            }
            Ct::Json => json_records(&batch.request).map_err(|e| format!("InvalidBatch : {e}"))?,
            Ct::Xml => xml_records(&batch.request).map_err(|e| format!("InvalidBatch : {e}"))?,
        };
        if recs.len() > 10_000 {
            return Err("InvalidBatch : Records not processed: a batch can contain at most 10,000 records".into());
        }
        batch.outcomes = process(st, ctx, obj, job.op, job.ext.as_deref(), recs)
            .map_err(|e| format!("InternalServerError : {e}"))?;
        batch.processed = batch.outcomes.len();
        batch.failed = batch.outcomes.iter().filter(|o| o.is_err()).count();
        Ok(())
    })();
    if let Err(msg) = result {
        batch.state = "Failed";
        batch.state_message = Some(msg);
    }
    batch.ms = start.elapsed().as_millis() as u64;
    batch.modstamp = st.ctx("", "").now;
    batch
}

fn dml_results(ct: Ct, json: bool, outcomes: &[Outcome]) -> Response {
    let errors_json = |e: &SfError| json!([{ "fields": e.fields, "message": e.message, "statusCode": e.code }]);
    if json || ct == Ct::Json {
        let list: Vec<Value> = outcomes
            .iter()
            .map(|o| match o {
                Ok((id, created)) => json!({ "success": true, "created": created, "id": id, "errors": [] }),
                Err(e) => json!({ "success": false, "created": false, "id": null, "errors": errors_json(e) }),
            })
            .collect();
        return json_resp(StatusCode::OK, json!(list));
    }
    if ct == Ct::Xml {
        let inner: String = outcomes
            .iter()
            .map(|o| match o {
                Ok((id, created)) => {
                    format!("<result><id>{id}</id><success>true</success><created>{created}</created></result>")
                }
                Err(e) => format!(
                    "<result><errors>{}<message>{}</message><statusCode>{}</statusCode></errors><success>false</success><created>false</created></result>",
                    e.fields.iter().map(|f| format!("<fields>{}</fields>", xml_escape(f))).collect::<String>(),
                    xml_escape(&e.message),
                    e.code
                ),
            })
            .collect();
        return xml_resp(StatusCode::OK, "results", &inner);
    }
    let mut out = String::new();
    csv_line(&mut out, ["Id", "Success", "Created", "Error"], ',', "\n");
    for o in outcomes {
        match o {
            Ok((id, created)) => csv_line(&mut out, [id.as_str(), "true", &created.to_string(), ""], ',', "\n"),
            Err(e) => csv_line(&mut out, ["", "false", "false", &error_text(e)], ',', "\n"),
        }
    }
    csv_resp(out)
}

/// Dates and datetimes in Bulk API 1.0's JSON query results are epoch milliseconds.
fn epoch_dates(cat: &Catalog, obj: Option<&SObject>, rec: &mut Value) {
    let Value::Object(m) = rec else { return };
    let obj =
        m.get("attributes").and_then(|a| a.get("type")).and_then(Value::as_str).and_then(|t| cat.object(t)).or(obj);
    let Some(obj) = obj else { return };
    for (k, v) in m.iter_mut() {
        if v.is_object() {
            let target = obj.relationship(k).and_then(|f| f.reference_to.as_deref()).and_then(|t| cat.object(t));
            epoch_dates(cat, target, v);
            continue;
        }
        let Some(s) = v.as_str() else { continue };
        let millis = match obj.field(k).map(|f| f.ty) {
            Some(FieldType::Datetime) => store::parse_datetime(s).map(|t| t.timestamp_millis()),
            Some(FieldType::Date) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis()),
            _ => None,
        };
        if let Some(ms) = millis {
            *v = json!(ms);
        }
    }
}

/// An XML `<records>` element (or a parent record inside one).
fn xml_record(tag: &str, rec: &Value) -> String {
    let Value::Object(m) = rec else { return String::new() };
    let ty = m.get("attributes").and_then(|a| a.get("type")).and_then(Value::as_str).unwrap_or_default();
    let mut out = format!(r#"<{tag} xsi:type="sObject"><type>{ty}</type>"#);
    for (k, v) in m.iter().filter(|(k, _)| *k != "attributes") {
        match v {
            Value::Null => out += &format!(r#"<{k} xsi:nil="true"/>"#),
            Value::Object(_) => out += &xml_record(k, v),
            Value::String(s) => out += &format!("<{k}>{}</{k}>", xml_escape(&csv_datetime(s))),
            other => out += &format!("<{k}>{other}</{k}>"),
        }
    }
    out + &format!("</{tag}>")
}

fn query_results(st: &AppState, ct: Ct, out: &QueryOutput) -> Response {
    match ct {
        Ct::Csv => csv_resp(query_csv(&out.header, &out.rows, ',', "\n")),
        Ct::Json => {
            let cat = st.cat();
            let mut records = out.records.clone();
            for r in &mut records {
                epoch_dates(&cat, cat.object(&out.object), r);
            }
            json_resp(StatusCode::OK, json!(records))
        }
        Ct::Xml => {
            let inner: String = out.records.iter().map(|r| xml_record("records", r)).collect();
            (
                [(header::CONTENT_TYPE, "application/xml")],
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?><queryResult xmlns="{NS}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">{inner}</queryResult>"#
                ),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trips_quotes_newlines_and_delimiters() {
        let rows =
            parse_csv("\u{feff}Name,Description\r\n\"Acme, Inc\",\"say \"\"hi\"\"\nthere\"\r\nBare,\n", ',').unwrap();
        assert_eq!(rows, vec![vec!["Name", "Description"], vec!["Acme, Inc", "say \"hi\"\nthere"], vec!["Bare", ""]]);
        assert_eq!(parse_csv("a|b\n1|2", '|').unwrap(), vec![vec!["a", "b"], vec!["1", "2"]]);
        assert!(parse_csv("a\n\"unterminated", ',').is_err());
        let mut out = String::new();
        csv_line(&mut out, ["a\"b", "c"], ';', "\r\n");
        assert_eq!(out, "\"a\"\"b\";\"c\"\r\n");
    }

    #[test]
    fn locators_are_base64_offsets() {
        assert_eq!(encode_locator(10000), "MTAwMDA");
        for n in [0, 1, 12, 123, 2000, 99999, 1234567] {
            assert_eq!(decode_locator(&encode_locator(n)), Some(n));
        }
        assert_eq!(decode_locator("!!"), None);
    }

    #[test]
    fn datetimes_end_in_z() {
        assert_eq!(csv_datetime("2026-01-31T09:30:00.000+0000"), "2026-01-31T09:30:00.000Z");
        assert_eq!(csv_datetime("2026-01-31"), "2026-01-31");
    }
}
