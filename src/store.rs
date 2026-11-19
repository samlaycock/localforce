//! Record storage (one SQLite table per sObject, one column per field) and
//! the DML rules Salesforce applies on the way in: field access, type
//! coercion, required fields, defaults, uniqueness, audit fields and
//! cascading deletes.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, params_from_iter};
use serde_json::{Map, Value, json};

use crate::error::{SfError, SfResult};
use crate::formula::{self, Env, Prior};
use crate::ids;
use crate::schema::{Catalog, Derived, Field, FieldType, OPPORTUNITY_STAGES, SObject};

pub fn open(path: Option<&Path>) -> anyhow::Result<Connection> {
    let conn = match path {
        Some(p) => Connection::open(p)?,
        None => Connection::open_in_memory()?,
    };
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE IF NOT EXISTS _meta (k TEXT PRIMARY KEY, v INTEGER NOT NULL);
         INSERT OR IGNORE INTO _meta VALUES ('seq', 0);
         CREATE TABLE IF NOT EXISTS _schema (kind TEXT NOT NULL, object TEXT NOT NULL, name TEXT NOT NULL, def TEXT NOT NULL,
                                             PRIMARY KEY (kind, object, name));
         CREATE TABLE IF NOT EXISTS _recent (user TEXT NOT NULL, object TEXT NOT NULL, id TEXT NOT NULL, viewed TEXT NOT NULL,
                                             PRIMARY KEY (user, id));",
    )?;
    formula::register_functions(&conn)?;
    Ok(conn)
}

/// Create the tables and columns the catalog needs. Tables and columns the
/// catalog no longer has are left alone, so a field removed from a config
/// file comes back with its data when it's restored.
pub fn sync_tables(conn: &Connection, cat: &Catalog) -> anyhow::Result<()> {
    for o in &cat.objects {
        let cols: Vec<String> = o.fields.iter().map(|f| format!("{} {}", qi(&f.name), f.ty.sql_type())).collect();
        conn.execute(
            &format!("CREATE TABLE IF NOT EXISTS {} ({}, PRIMARY KEY (\"Id\"))", qi(&o.name), cols.join(", ")),
            [],
        )?;
        // Fields added to the config since the database was created.
        let existing: HashSet<String> = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{}')", o.name))?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        for f in &o.fields {
            if !existing.contains(&f.name) {
                conn.execute(
                    &format!("ALTER TABLE {} ADD COLUMN {} {}", qi(&o.name), qi(&f.name), f.ty.sql_type()),
                    [],
                )?;
            }
        }
    }
    create_name_view(conn, cat)?;
    Ok(())
}

/// `_Name`: every record a polymorphic lookup can refer to, with the fields
/// of the `Name` object (`What.Name`, `Who.Email`, …).
fn create_name_view(conn: &Connection, cat: &Catalog) -> anyhow::Result<()> {
    let mut targets: Vec<&SObject> = vec![];
    for f in cat.objects.iter().flat_map(|o| &o.fields).filter(|f| f.is_polymorphic()) {
        for t in f.targets() {
            let o = cat.object(t).unwrap();
            if !targets.iter().any(|x| x.name == o.name) {
                targets.push(o);
            }
        }
    }
    let selects: Vec<String> = targets
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let cols: Vec<String> = cat
                .name_object
                .fields
                .iter()
                .map(|nf| {
                    let value = match nf.name.as_str() {
                        "Type" => format!("'{}'", o.name),
                        "Name" => o.name_field().map(|f| qi(&f.name)).unwrap_or_else(|| "NULL".into()),
                        name => match o.field(name) {
                            Some(f) if !f.is_computed() => qi(&f.name),
                            _ => "NULL".into(),
                        },
                    };
                    let collate = if i == 0 && nf.ty.is_text() { " COLLATE NOCASE" } else { "" };
                    format!("{value}{collate} AS {}", qi(&nf.name))
                })
                .collect();
            format!("SELECT {} FROM {}", cols.join(", "), qi(&o.name))
        })
        .collect();
    conn.execute_batch("DROP VIEW IF EXISTS \"_Name\"")?;
    if !selects.is_empty() {
        conn.execute_batch(&format!("CREATE VIEW \"_Name\" AS {}", selects.join(" UNION ALL ")))?;
    }
    Ok(())
}

/// Quote an identifier for SQLite.
pub fn qi(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Who is making the change, and when.
#[derive(Clone)]
pub struct Ctx {
    pub user_id: String,
    pub now: DateTime<Utc>,
    pub api_version: String,
}

impl Ctx {
    pub fn now_str(&self) -> String {
        format_datetime(self.now)
    }
}

pub fn format_datetime(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3f+0000").to_string()
}

pub fn parse_datetime(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%z"] {
        if let Ok(t) = DateTime::parse_from_str(s, fmt) {
            return Some(t.with_timezone(&Utc));
        }
    }
    // A bare date is midnight UTC, as Salesforce reads it.
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

fn bad_type(f: &Field, v: &Value) -> SfError {
    let shown = match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    SfError::new(
        "JSON_PARSER_ERROR",
        format!("Cannot deserialize instance of {} from value {shown}", f.ty.describe_name()),
    )
    .with_fields(vec![f.name.clone()])
}

/// Coerce a JSON value from a request into the column value for `f`.
pub fn to_sql(f: &Field, v: &Value) -> SfResult<Sql> {
    if v.is_null() {
        return Ok(Sql::Null);
    }
    let text = || -> SfResult<String> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            _ => Err(bad_type(f, v)),
        }
    };
    Ok(match f.ty {
        FieldType::Boolean => match v {
            Value::Bool(b) => Sql::Integer(*b as i64),
            Value::String(s) if s.eq_ignore_ascii_case("true") => Sql::Integer(1),
            Value::String(s) if s.eq_ignore_ascii_case("false") => Sql::Integer(0),
            _ => return Err(bad_type(f, v)),
        },
        FieldType::Int => match v {
            Value::Number(n) if n.is_i64() => Sql::Integer(n.as_i64().unwrap()),
            Value::String(s) => Sql::Integer(s.trim().parse().map_err(|_| bad_type(f, v))?),
            _ => return Err(bad_type(f, v)),
        },
        FieldType::Double | FieldType::Currency | FieldType::Percent => match v {
            Value::Number(n) => Sql::Real(n.as_f64().unwrap()),
            Value::String(s) => Sql::Real(s.trim().parse().map_err(|_| bad_type(f, v))?),
            _ => return Err(bad_type(f, v)),
        },
        FieldType::Date => {
            let s = text()?;
            let d = NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .ok()
                .or_else(|| parse_datetime(&s).map(|t| t.date_naive()))
                .ok_or_else(|| bad_type(f, v))?;
            Sql::Text(d.format("%Y-%m-%d").to_string())
        }
        FieldType::Datetime => Sql::Text(format_datetime(parse_datetime(&text()?).ok_or_else(|| bad_type(f, v))?)),
        FieldType::Id | FieldType::Reference => {
            let s = text()?;
            if s.is_empty() {
                return Ok(Sql::Null);
            }
            Sql::Text(ids::to18(&s).ok_or_else(|| {
                SfError::new("MALFORMED_ID", format!("{}: id value of incorrect type: {s}", f.label))
                    .with_fields(vec![f.name.clone()])
            })?)
        }
        _ => {
            let s = text()?;
            if s.is_empty() {
                return Ok(Sql::Null);
            }
            if let Some(max) = f.length
                && s.chars().count() > max as usize
            {
                return Err(SfError::new(
                    "STRING_TOO_LONG",
                    format!("{}: data value too large: {s} (max length={max})", f.label),
                )
                .with_fields(vec![f.name.clone()]));
            }
            if f.ty == FieldType::Email && !(s.contains('@') && !s.starts_with('@') && !s.ends_with('@')) {
                return Err(SfError::new("INVALID_EMAIL_ADDRESS", format!("{}: invalid email address: {s}", f.label))
                    .with_fields(vec![f.name.clone()]));
            }
            if f.restricted {
                let parts: Vec<&str> = if f.ty == FieldType::Multipicklist { s.split(';').collect() } else { vec![&s] };
                for p in parts {
                    if !f.picklist.iter().any(|v| v == p) {
                        return Err(SfError::new(
                            "INVALID_OR_NULL_FOR_RESTRICTED_PICKLIST",
                            format!("{}: bad value for restricted picklist field: {p}", f.label),
                        )
                        .with_fields(vec![f.name.clone()]));
                    }
                }
            }
            Sql::Text(s)
        }
    })
}

/// Turn a column value back into the JSON Salesforce returns.
pub fn from_sql(f: &Field, v: Sql) -> Value {
    match (f.ty, v) {
        (_, Sql::Null) => Value::Null,
        (FieldType::Address, Sql::Text(s)) => serde_json::from_str(&s).unwrap_or(Value::Null),
        (FieldType::Boolean, Sql::Integer(i)) => Value::Bool(i != 0),
        (FieldType::Int, Sql::Integer(i)) => json!(i),
        (_, Sql::Integer(i)) => json!(i as f64),
        (_, Sql::Real(r)) => json!(r),
        (_, Sql::Text(s)) => Value::String(s),
        (_, Sql::Blob(_)) => Value::Null,
    }
}

pub fn record_url(ctx: &Ctx, object: &str, id: &str) -> String {
    format!("/services/data/v{}/sobjects/{object}/{id}", ctx.api_version)
}

pub fn attributes(ctx: &Ctx, object: &str, id: Option<&str>) -> Value {
    match id {
        Some(id) => json!({ "type": object, "url": record_url(ctx, object, id) }),
        None => json!({ "type": object }),
    }
}

fn next_seq(conn: &Connection) -> SfResult<u64> {
    Ok(conn.query_row("UPDATE _meta SET v = v + 1 WHERE k = 'seq' RETURNING v", [], |r| r.get::<_, i64>(0))? as u64)
}

/// Resolve and coerce the fields in a request body, applying field-level access.
fn writable_values(obj: &SObject, body: &Map<String, Value>, insert: bool) -> SfResult<Vec<(Field, Sql)>> {
    let mut out = vec![];
    let mut not_writable = vec![];
    for (k, v) in body {
        if k == "attributes" || (!insert && k.eq_ignore_ascii_case("Id")) {
            continue;
        }
        let f = obj.field(k).ok_or_else(|| {
            SfError::new("INVALID_FIELD", format!("No such column '{k}' on sobject of type {}", obj.name))
                .with_fields(vec![k.clone()])
        })?;
        if (insert && !f.createable) || (!insert && !f.updateable) {
            not_writable.push(f.name.clone());
            continue;
        }
        out.push((f.clone(), to_sql(f, v)?));
    }
    if !not_writable.is_empty() {
        return Err(SfError::new(
            "INVALID_FIELD_FOR_INSERT_UPDATE",
            format!(
                "Unable to create/update fields: {}. Please check the security settings of this field and verify that it is read/write for your profile or permission set.",
                not_writable.join(", ")
            ),
        )
        .with_fields(not_writable));
    }
    Ok(out)
}

fn check_references(conn: &Connection, cat: &Catalog, values: &[(Field, Sql)]) -> SfResult<()> {
    for (f, v) in values {
        let (Some(to), Sql::Text(id), Derived::None) = (&f.reference_to, v, &f.derived) else { continue };
        // A polymorphic lookup refers to whichever of its targets the id belongs to.
        let target = if f.is_polymorphic() {
            cat.object_for_id(id).filter(|o| f.targets().any(|t| t.eq_ignore_ascii_case(&o.name)))
        } else {
            cat.object(to)
        };
        let deleted: Option<bool> = target.and_then(|target| {
            conn.query_row(&format!("SELECT \"IsDeleted\" FROM {} WHERE \"Id\" = ?1", qi(&target.name)), [id], |r| {
                r.get(0)
            })
            .ok()
        });
        if deleted == Some(true) {
            return Err(SfError::new("ENTITY_IS_DELETED", "entity is deleted").with_fields(vec![f.name.clone()]));
        }
        if deleted.is_none() {
            let wrong_type =
                cat.object_for_id(id).is_some_and(|o| !f.targets().any(|t| o.name.eq_ignore_ascii_case(t)));
            let msg = if wrong_type {
                format!("{}: id value of incorrect type: {id}", f.label)
            } else {
                "invalid cross reference id".to_string()
            };
            let code = if wrong_type { "FIELD_INTEGRITY_EXCEPTION" } else { "INVALID_CROSS_REFERENCE_KEY" };
            return Err(SfError::new(code, msg).with_fields(vec![f.name.clone()]));
        }
    }
    Ok(())
}

fn check_unique(conn: &Connection, obj: &SObject, values: &[(Field, Sql)], self_id: Option<&str>) -> SfResult<()> {
    for (f, v) in values {
        if !f.unique || matches!(v, Sql::Null) {
            continue;
        }
        let other: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT \"Id\" FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 0 AND \"Id\" IS NOT ?2",
                    qi(&obj.name),
                    qi(&f.name)
                ),
                rusqlite::params![v, self_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(other) = other {
            return Err(SfError::new(
                "DUPLICATE_VALUE",
                format!("duplicate value found: {} duplicates value on record with id: {other}", f.name),
            )
            .with_fields(vec![f.name.clone()]));
        }
    }
    Ok(())
}

fn full_name(first: Option<&str>, last: Option<&str>) -> Sql {
    let parts: Vec<&str> = [first, last].into_iter().flatten().filter(|s| !s.is_empty()).collect();
    if parts.is_empty() { Sql::Null } else { Sql::Text(parts.join(" ")) }
}

fn text_of(values: &[(Field, Sql)], name: &str) -> Option<String> {
    values.iter().find(|(f, _)| f.name == name).and_then(|(_, v)| match v {
        Sql::Text(s) => Some(s.clone()),
        _ => None,
    })
}

pub fn insert(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    body: &Map<String, Value>,
) -> SfResult<String> {
    let mut values = writable_values(obj, body, true)?;
    let now = Sql::Text(ctx.now_str());
    let me = Sql::Text(ctx.user_id.clone());
    let id = ids::make(&obj.key_prefix, next_seq(conn)?);
    let given: HashSet<String> = values.iter().map(|(f, _)| f.name.clone()).collect();

    for f in &obj.fields {
        if given.contains(&f.name) {
            continue;
        }
        let v = match (&f.derived, f.name.as_str()) {
            (Derived::System, "Id") => Sql::Text(id.clone()),
            (Derived::System, "IsDeleted") => Sql::Integer(0),
            (Derived::System, "CreatedById" | "LastModifiedById") => me.clone(),
            (Derived::System, _) => now.clone(),
            (Derived::FullName, _) => {
                full_name(text_of(&values, "FirstName").as_deref(), text_of(&values, "LastName").as_deref())
            }
            (Derived::AutoNumber(format), _) => {
                let key = format!("auto:{}.{}", obj.name, f.name);
                conn.execute("INSERT OR IGNORE INTO _meta VALUES (?1, 0)", [&key])?;
                let n: i64 =
                    conn.query_row("UPDATE _meta SET v = v + 1 WHERE k = ?1 RETURNING v", [&key], |r| r.get(0))?;
                Sql::Text(crate::schema::format_auto_number(format, n, ctx.now))
            }
            (Derived::Formula(_) | Derived::Rollup(_) | Derived::Logic | Derived::Address(_), _) => continue,
            (Derived::None, "OwnerId") => me.clone(),
            (Derived::None, _) if f.default_formula.is_some() => {
                let env = Env { cat, user_id: &ctx.user_id, now: ctx.now, prior: Prior::New };
                let src = f.default_formula.as_deref().unwrap();
                match crate::formula::default_value(conn, &env, obj, f, src) {
                    Ok(Sql::Null) => continue,
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("default value of {}.{}: {e}", obj.name, f.name);
                        continue;
                    }
                }
            }
            (Derived::None, _) => match (&f.default, f.ty) {
                (Some(d), _) => to_sql(f, d)?,
                (None, FieldType::Boolean) => Sql::Integer(0),
                _ => continue,
            },
        };
        values.push((f.clone(), v));
    }

    let missing: Vec<String> = obj
        .fields
        .iter()
        .filter(|f| f.required && values.iter().all(|(v, x)| v.name != f.name || matches!(x, Sql::Null)))
        .map(|f| f.name.clone())
        .collect();
    if !missing.is_empty() {
        return Err(SfError::new(
            "REQUIRED_FIELD_MISSING",
            format!("Required fields are missing: [{}]", missing.join(", ")),
        )
        .with_fields(missing));
    }
    check_references(conn, cat, &values)?;
    standard_logic(conn, cat, ctx, obj, &mut values, None)?;
    check_unique(conn, obj, &values, None)?;

    let cols: Vec<String> = values.iter().map(|(f, _)| qi(&f.name)).collect();
    let marks: Vec<String> = (1..=values.len()).map(|i| format!("?{i}")).collect();
    in_savepoint(conn, || {
        conn.execute(
            &format!("INSERT INTO {} ({}) VALUES ({})", qi(&obj.name), cols.join(", "), marks.join(", ")),
            params_from_iter(values.iter().map(|(_, v)| v)),
        )?;
        crate::sales::after_save(conn, cat, ctx, obj, &id, None, false)?;
        validate(conn, cat, ctx, obj, &id, Prior::New)
    })?;
    Ok(id)
}

/// Run `f` in a savepoint, undoing its writes if it fails.
fn in_savepoint<T>(conn: &Connection, f: impl FnOnce() -> SfResult<T>) -> SfResult<T> {
    conn.execute_batch("SAVEPOINT lf_dml")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE lf_dml")?;
            Ok(v)
        }
        Err(e) => {
            conn.execute_batch("ROLLBACK TO lf_dml; RELEASE lf_dml")?;
            Err(e)
        }
    }
}

/// Check the object's active validation rules against the saved row.
fn validate(conn: &Connection, cat: &Catalog, ctx: &Ctx, obj: &SObject, id: &str, prior: Prior) -> SfResult<()> {
    for rule in obj.validation_rules.iter().filter(|r| r.active) {
        let env = Env { cat, user_id: &ctx.user_id, now: ctx.now, prior };
        let cond = match formula::condition_sql(&env, obj, &rule.condition, "r") {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("validation rule {}.{} ignored: {e}", obj.name, rule.name);
                continue;
            }
        };
        let hit: i64 =
            conn.query_row(&format!("SELECT {cond} FROM {} r WHERE r.\"Id\" = ?1", qi(&obj.name)), [id], |r| {
                r.get::<_, Option<i64>>(0).map(|v| v.unwrap_or(0))
            })?;
        if hit != 0 {
            return Err(SfError::new("FIELD_CUSTOM_VALIDATION_EXCEPTION", rule.message.clone())
                .with_fields(rule.field.iter().cloned().collect()));
        }
    }
    Ok(())
}

/// A record's stored values, by field name.
fn stored_row(conn: &Connection, obj: &SObject, id: &str) -> SfResult<HashMap<String, Sql>> {
    let fields: Vec<&Field> = obj.fields.iter().filter(|f| !f.is_computed()).collect();
    let cols: Vec<String> = fields.iter().map(|f| qi(&f.name)).collect();
    let row: Vec<Sql> =
        conn.query_row(&format!("SELECT {} FROM {} WHERE \"Id\" = ?1", cols.join(", "), qi(&obj.name)), [id], |r| {
            (0..fields.len()).map(|i| r.get::<_, Sql>(i)).collect()
        })?;
    Ok(fields.iter().map(|f| f.name.clone()).zip(row).collect())
}

/// Set a value, replacing what's there.
pub(crate) fn set(obj: &SObject, values: &mut Vec<(Field, Sql)>, name: &str, v: Sql) {
    values.retain(|(f, _)| f.name != name);
    values.push((obj.field(name).unwrap().clone(), v));
}

/// The value a field will have: what's being written, or what's stored.
pub(crate) fn value_of(values: &[(Field, Sql)], old: Option<&HashMap<String, Sql>>, name: &str) -> Sql {
    values
        .iter()
        .find(|(f, _)| f.name == name)
        .map(|(_, v)| v.clone())
        .or_else(|| old.and_then(|o| o.get(name).cloned()))
        .unwrap_or(Sql::Null)
}

pub(crate) fn given(values: &[(Field, Sql)], name: &str) -> bool {
    values.iter().any(|(f, _)| f.name == name)
}

pub(crate) fn bool_sql(b: bool) -> Sql {
    Sql::Integer(b as i64)
}

/// What the standard objects do for themselves on save: an Opportunity's
/// stage sets IsClosed, IsWon, Probability and ForecastCategoryName; a closed
/// Case gets a ClosedDate; a Task is about an Account; an Event's start, end
/// and duration agree; a price book entry needs a standard price first.
fn standard_logic(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    values: &mut Vec<(Field, Sql)>,
    old: Option<&HashMap<String, Sql>>,
) -> SfResult<()> {
    let insert = old.is_none();
    let text = |values: &[(Field, Sql)], name: &str| sql_text(&value_of(values, old, name)).unwrap_or_default();
    match obj.name.as_str() {
        "Opportunity" if insert || given(values, "StageName") => {
            let stage_name = text(values, "StageName");
            if let Some(stage) = OPPORTUNITY_STAGES.iter().find(|s| s.name.eq_ignore_ascii_case(&stage_name)) {
                let changed = insert || old.and_then(|o| o.get("StageName")).and_then(sql_text) != Some(stage_name);
                set(obj, values, "IsClosed", bool_sql(stage.closed));
                set(obj, values, "IsWon", bool_sql(stage.won));
                if changed && !given(values, "Probability") {
                    set(obj, values, "Probability", Sql::Real(stage.probability));
                }
                if changed && !given(values, "ForecastCategoryName") {
                    set(obj, values, "ForecastCategoryName", Sql::Text(stage.forecast.into()));
                }
            } else if insert {
                set(obj, values, "IsClosed", bool_sql(false));
                set(obj, values, "IsWon", bool_sql(false));
            }
        }
        "Case" if insert || given(values, "Status") => {
            let closed = text(values, "Status").eq_ignore_ascii_case("Closed");
            let was_closed = old.and_then(|o| o.get("IsClosed")) == Some(&Sql::Integer(1));
            set(obj, values, "IsClosed", bool_sql(closed));
            if closed != was_closed || insert {
                set(obj, values, "ClosedDate", if closed { Sql::Text(ctx.now_str()) } else { Sql::Null });
            }
        }
        "Task" | "Event" if is_lead(&text(values, "WhoId")) && !text(values, "WhatId").is_empty() => {
            return Err(SfError::new(
                "FIELD_INTEGRITY_EXCEPTION",
                "Related To ID: You can't relate an activity about a lead to another record.",
            )
            .with_fields(vec!["WhatId".into()]));
        }
        "Task" => {
            if insert || given(values, "Status") {
                let done = text(values, "Status").eq_ignore_ascii_case("Completed");
                let was_done = old.and_then(|o| o.get("IsClosed")) == Some(&Sql::Integer(1));
                set(obj, values, "IsClosed", bool_sql(done));
                if done != was_done || insert {
                    set(obj, values, "CompletedDateTime", if done { Sql::Text(ctx.now_str()) } else { Sql::Null });
                }
            }
            if insert || given(values, "Priority") {
                set(obj, values, "IsHighPriority", bool_sql(text(values, "Priority").eq_ignore_ascii_case("High")));
            }
            if insert || given(values, "WhatId") || given(values, "WhoId") {
                let account = activity_account(conn, cat, &text(values, "WhatId"), &text(values, "WhoId"));
                set(obj, values, "AccountId", account.map(Sql::Text).unwrap_or(Sql::Null));
            }
        }
        "Event" => {
            if insert || given(values, "WhatId") || given(values, "WhoId") {
                let account = activity_account(conn, cat, &text(values, "WhatId"), &text(values, "WhoId"));
                set(obj, values, "AccountId", account.map(Sql::Text).unwrap_or(Sql::Null));
            }
            event_times(obj, values, old)?;
        }
        "Pricebook2" if insert => set(obj, values, "IsStandard", bool_sql(false)),
        "PricebookEntry" if insert => {
            let product = text(values, "Product2Id");
            let book = text(values, "Pricebook2Id");
            let (name, code): (Option<String>, Option<String>) = conn
                .query_row("SELECT \"Name\", \"ProductCode\" FROM \"Product2\" WHERE \"Id\" = ?1", [&product], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap_or_default();
            set(obj, values, "Name", name.map(Sql::Text).unwrap_or(Sql::Null));
            set(obj, values, "ProductCode", code.map(Sql::Text).unwrap_or(Sql::Null));
            let standard: bool = conn
                .query_row("SELECT \"IsStandard\" FROM \"Pricebook2\" WHERE \"Id\" = ?1", [&book], |r| r.get(0))
                .unwrap_or(false);
            let existing = |book_filter: &str| -> SfResult<Option<String>> {
                Ok(conn
                    .query_row(
                        &format!(
                            "SELECT e.\"Id\" FROM \"PricebookEntry\" e JOIN \"Pricebook2\" b ON b.\"Id\" = e.\"Pricebook2Id\"
                             WHERE e.\"Product2Id\" = ?1 AND e.\"IsDeleted\" = 0 AND {book_filter}"
                        ),
                        [&product, &book],
                        |r| r.get(0),
                    )
                    .ok())
            };
            if existing("e.\"Pricebook2Id\" = ?2")?.is_some() {
                return Err(SfError::new("DUPLICATE_VALUE", "This price definition already exists in this price book"));
            }
            if !standard && existing("b.\"IsStandard\" = 1 AND ?2 IS NOT NULL")?.is_none() {
                return Err(SfError::new("STANDARD_PRICE_NOT_DEFINED", "No standard price defined for this product"));
            }
        }
        _ => {}
    }
    crate::sales::before_save(conn, cat, ctx, obj, values, old)
}

fn is_lead(id: &str) -> bool {
    id.starts_with("00Q")
}

/// The Account a task or event is about: its What if that's an Account (or
/// belongs to one), otherwise its Who's Account.
fn activity_account(conn: &Connection, cat: &Catalog, what: &str, who: &str) -> Option<String> {
    let account_of = |id: &str| -> Option<String> {
        let o = cat.object_for_id(id)?;
        if o.name == "Account" {
            return Some(id.to_string());
        }
        o.field("AccountId")?;
        conn.query_row(&format!("SELECT \"AccountId\" FROM {} WHERE \"Id\" = ?1", qi(&o.name)), [id], |r| r.get(0))
            .ok()
            .flatten()
    };
    account_of(what).or_else(|| account_of(who))
}

/// An Event's StartDateTime, ActivityDateTime, EndDateTime, DurationInMinutes and ActivityDate.
fn event_times(obj: &SObject, values: &mut Vec<(Field, Sql)>, old: Option<&HashMap<String, Sql>>) -> SfResult<()> {
    let time =
        |values: &[(Field, Sql)], name: &str| sql_text(&value_of(values, old, name)).and_then(|s| parse_datetime(&s));
    let start = if given(values, "ActivityDateTime") && !given(values, "StartDateTime") {
        time(values, "ActivityDateTime")
    } else {
        time(values, "StartDateTime").or_else(|| time(values, "ActivityDateTime"))
    };
    let Some(start) = start else {
        return Err(SfError::new("REQUIRED_FIELD_MISSING", "Required fields are missing: [ActivityDateTime]")
            .with_fields(vec!["ActivityDateTime".into()]));
    };
    let minutes = match value_of(values, old, "DurationInMinutes") {
        Sql::Integer(n) => Some(n),
        _ => None,
    };
    let end_given = given(values, "EndDateTime");
    let end = match (time(values, "EndDateTime"), minutes) {
        (Some(end), Some(_)) if end_given => end,
        (_, Some(m)) => start + chrono::Duration::minutes(m),
        (Some(end), None) => end,
        (None, None) => {
            return Err(SfError::new("REQUIRED_FIELD_MISSING", "Required fields are missing: [DurationInMinutes]")
                .with_fields(vec!["DurationInMinutes".into()]));
        }
    };
    if end < start {
        return Err(SfError::new(
            "FIELD_INTEGRITY_EXCEPTION",
            "End Date/Time: The end time can't be before the start time.",
        )
        .with_fields(vec!["EndDateTime".into()]));
    }
    set(obj, values, "StartDateTime", Sql::Text(format_datetime(start)));
    set(obj, values, "ActivityDateTime", Sql::Text(format_datetime(start)));
    set(obj, values, "EndDateTime", Sql::Text(format_datetime(end)));
    set(obj, values, "DurationInMinutes", Sql::Integer((end - start).num_minutes()));
    set(obj, values, "ActivityDate", Sql::Text(start.format("%Y-%m-%d").to_string()));
    Ok(())
}

/// Look up a live record, distinguishing "never existed" from "in the recycle bin".
fn require_live(conn: &Connection, obj: &SObject, id: &str) -> SfResult<String> {
    let id18 = ids::to18(id).ok_or_else(SfError::not_found)?;
    let deleted: Option<bool> = conn
        .query_row(&format!("SELECT \"IsDeleted\" FROM {} WHERE \"Id\" = ?1", qi(&obj.name)), [&id18], |r| r.get(0))
        .ok();
    match deleted {
        None => Err(SfError::not_found()),
        Some(true) => Err(SfError::new("ENTITY_IS_DELETED", "entity is deleted")),
        Some(false) => Ok(id18),
    }
}

pub fn update(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    id: &str,
    body: &Map<String, Value>,
) -> SfResult<()> {
    let id = require_live(conn, obj, id)?;
    let mut values = writable_values(obj, body, false)?;
    let old = stored_row(conn, obj, &id)?;
    let nulled: Vec<String> =
        values.iter().filter(|(f, v)| f.required && matches!(v, Sql::Null)).map(|(f, _)| f.name.clone()).collect();
    if !nulled.is_empty() {
        return Err(SfError::new(
            "REQUIRED_FIELD_MISSING",
            format!("Required fields are missing: [{}]", nulled.join(", ")),
        )
        .with_fields(nulled));
    }
    check_references(conn, cat, &values)?;
    standard_logic(conn, cat, ctx, obj, &mut values, Some(&old))?;
    check_unique(conn, obj, &values, Some(&id))?;

    if let Some(full) = obj.fields.iter().find(|f| f.derived == Derived::FullName) {
        let touched = values.iter().any(|(f, _)| f.name == "FirstName" || f.name == "LastName");
        if touched {
            let (first, last): (Option<String>, Option<String>) = conn.query_row(
                &format!("SELECT \"FirstName\", \"LastName\" FROM {} WHERE \"Id\" = ?1", qi(&obj.name)),
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let first = values.iter().find(|(f, _)| f.name == "FirstName").map(|(_, v)| sql_text(v)).unwrap_or(first);
            let last = values.iter().find(|(f, _)| f.name == "LastName").map(|(_, v)| sql_text(v)).unwrap_or(last);
            values.push((full.clone(), full_name(first.as_deref(), last.as_deref())));
        }
    }
    for name in ["LastModifiedDate", "SystemModstamp"] {
        values.push((obj.field(name).unwrap().clone(), Sql::Text(ctx.now_str())));
    }
    values.push((obj.field("LastModifiedById").unwrap().clone(), Sql::Text(ctx.user_id.clone())));

    let sets: Vec<String> =
        values.iter().enumerate().map(|(i, (f, _))| format!("{} = ?{}", qi(&f.name), i + 2)).collect();
    let mut params: Vec<&Sql> = vec![];
    let id_sql = Sql::Text(id.clone());
    params.push(&id_sql);
    params.extend(values.iter().map(|(_, v)| v));
    in_savepoint(conn, || {
        conn.execute(
            &format!("UPDATE {} SET {} WHERE \"Id\" = ?1", qi(&obj.name), sets.join(", ")),
            params_from_iter(params),
        )?;
        crate::sales::after_save(conn, cat, ctx, obj, &id, Some(&old), false)?;
        validate(conn, cat, ctx, obj, &id, Prior::Old(&old))
    })
}

pub(crate) fn sql_text(v: &Sql) -> Option<String> {
    match v {
        Sql::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Move a record to the recycle bin. Master-detail (and the standard
/// Account) children go with it; other lookups to it are cleared.
pub fn delete(conn: &Connection, cat: &Catalog, ctx: &Ctx, obj: &SObject, id: &str) -> SfResult<()> {
    let id = require_live(conn, obj, id)?;
    if obj.name == "Pricebook2"
        && conn.query_row("SELECT \"IsStandard\" FROM \"Pricebook2\" WHERE \"Id\" = ?1", [&id], |r| r.get(0))
            == Ok(true)
    {
        return Err(SfError::new("DELETE_FAILED", "You can't delete the standard price book."));
    }
    crate::sales::before_delete(conn, obj, &id)?;
    in_savepoint(conn, || soft_delete(conn, cat, ctx, obj, &id))
}

fn soft_delete(conn: &Connection, cat: &Catalog, ctx: &Ctx, obj: &SObject, id: &str) -> SfResult<()> {
    for rel in cat.children_of(&obj.name).iter().filter(|r| r.restrict_delete) {
        let child = cat.object(&rel.child_object).unwrap();
        let blocking: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT \"Id\" FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 0 LIMIT 1",
                    qi(&child.name),
                    qi(&rel.field)
                ),
                [id],
                |r| r.get(0),
            )
            .ok();
        if let Some(b) = blocking {
            return Err(SfError::new(
                "DELETE_FAILED",
                format!(
                    "Your attempt to delete {} {id} could not be completed because it is associated with the following {}: {b}",
                    obj.label, child.label_plural
                ),
            ));
        }
    }
    conn.execute(
        &format!("UPDATE {} SET \"IsDeleted\" = 1, \"SystemModstamp\" = ?2 WHERE \"Id\" = ?1", qi(&obj.name)),
        [id, &ctx.now_str()],
    )?;
    for rel in cat.children_of(&obj.name) {
        let child = cat.object(&rel.child_object).unwrap();
        let table = qi(&child.name);
        let col = qi(&rel.field);
        if rel.cascade_delete {
            let ids: Vec<String> = conn
                .prepare(&format!("SELECT \"Id\" FROM {table} WHERE {col} = ?1 AND \"IsDeleted\" = 0"))?
                .query_map([id], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            for child_id in ids {
                soft_delete(conn, cat, ctx, child, &child_id)?;
            }
        } else {
            conn.execute(&format!("UPDATE {table} SET {col} = NULL WHERE {col} = ?1"), [id])?;
        }
    }
    crate::sales::after_save(conn, cat, ctx, obj, id, None, false)
}

/// Restore a record from the recycle bin, with the children its deletion cascaded to.
pub fn undelete(conn: &Connection, cat: &Catalog, ctx: &Ctx, obj: &SObject, id: &str) -> SfResult<()> {
    let id = ids::to18(id).ok_or_else(|| SfError::new("MALFORMED_ID", format!("malformed id {id}")))?;
    let deleted: Option<bool> = conn
        .query_row(&format!("SELECT \"IsDeleted\" FROM {} WHERE \"Id\" = ?1", qi(&obj.name)), [&id], |r| r.get(0))
        .ok();
    match deleted {
        None => return Err(SfError::new("INVALID_ID_FIELD", "invalid record id")),
        Some(false) => return Err(SfError::new("UNDELETE_FAILED", "Entity is not in the recycle bin")),
        Some(true) => {}
    }
    in_savepoint(conn, || restore(conn, cat, ctx, obj, &id))
}

fn restore(conn: &Connection, cat: &Catalog, ctx: &Ctx, obj: &SObject, id: &str) -> SfResult<()> {
    conn.execute(
        &format!("UPDATE {} SET \"IsDeleted\" = 0, \"SystemModstamp\" = ?2 WHERE \"Id\" = ?1", qi(&obj.name)),
        [id, &ctx.now_str()],
    )?;
    for rel in cat.children_of(&obj.name).iter().filter(|r| r.cascade_delete) {
        let child = cat.object(&rel.child_object).unwrap();
        let ids: Vec<String> = conn
            .prepare(&format!(
                "SELECT \"Id\" FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 1",
                qi(&child.name),
                qi(&rel.field)
            ))?
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for child_id in ids {
            restore(conn, cat, ctx, child, &child_id)?;
        }
    }
    crate::sales::after_save(conn, cat, ctx, obj, id, None, false)
}

/// Delete every record except users, for a fresh start.
pub fn reset(conn: &Connection, cat: &Catalog) -> SfResult<()> {
    for o in cat.objects.iter().filter(|o| o.name != "User") {
        conn.execute(&format!("DELETE FROM {}", qi(&o.name)), [])?;
    }
    conn.execute("DELETE FROM _meta WHERE k LIKE 'auto:%'", [])?;
    conn.execute("DELETE FROM _recent", [])?;
    Ok(())
}

/// Read one record. `fields` limits the fields returned (the `?fields=` parameter).
pub fn get(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    obj: &SObject,
    id: &str,
    fields: Option<&[String]>,
) -> SfResult<Value> {
    let id = require_live(conn, obj, id)?;
    let chosen: Vec<&Field> = match fields {
        None => obj.fields.iter().collect(),
        Some(names) => {
            let mut wanted: Vec<&str> = names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
            // Id is always returned, after the fields asked for.
            if !wanted.iter().any(|n| n.eq_ignore_ascii_case("Id")) {
                wanted.push("Id");
            }
            wanted
                .into_iter()
                .map(|n| {
                    obj.field(n).ok_or_else(|| {
                        SfError::new("INVALID_FIELD", format!("No such column '{n}' on entity '{}'.", obj.name))
                    })
                })
                .collect::<SfResult<_>>()?
        }
    };
    let env = Env { cat, user_id: &ctx.user_id, now: ctx.now, prior: Prior::None };
    let cols: Vec<String> =
        chosen
            .iter()
            .map(|f| {
                if f.is_computed() {
                    formula::field_sql_or_null(&env, obj, f, "r")
                } else {
                    format!("r.{}", qi(&f.name))
                }
            })
            .collect();
    let row: Vec<Sql> = conn.query_row(
        &format!("SELECT {} FROM {} r WHERE r.\"Id\" = ?1", cols.join(", "), qi(&obj.name)),
        [&id],
        |r| (0..chosen.len()).map(|i| r.get::<_, Sql>(i)).collect(),
    )?;
    let mut rec = Map::new();
    rec.insert("attributes".into(), attributes(ctx, &obj.name, Some(&id)));
    for (f, v) in chosen.iter().zip(row) {
        rec.insert(f.name.clone(), from_sql(f, v));
    }
    Ok(Value::Object(rec))
}

/// Find live records by an external id field (or `Id`).
pub fn find_by(conn: &Connection, obj: &SObject, field: &Field, value: &str) -> SfResult<Vec<String>> {
    let v = to_sql(field, &Value::String(value.into()))?;
    Ok(conn
        .prepare(&format!("SELECT \"Id\" FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 0", qi(&obj.name), qi(&field.name)))?
        .query_map([v], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}
