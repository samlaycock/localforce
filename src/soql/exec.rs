//! Compiles a parsed SOQL query to SQLite SQL, runs it, and shapes the rows
//! into Salesforce's JSON (nested parent objects, child subquery results,
//! `AggregateResult` records).

use std::collections::HashMap;

use chrono::{Datelike, Duration, Months, NaiveDate};
use rusqlite::Connection;
use rusqlite::types::Value as Sql;
use serde_json::{Map, Value, json};

use super::parse::{Cond, Expr, GroupMode, InSource, Literal, Query, SelectItem, query_error};
use crate::error::{SfError, SfResult};
use crate::formula::{self, Env, Prior};
use crate::ids;
use crate::schema::{Catalog, Field, FieldType, SObject};
use crate::store::{Ctx, attributes, format_datetime, from_sql, parse_datetime, qi};

pub struct Output {
    pub total_size: usize,
    pub records: Vec<Value>,
}

/// Run a query. `all_rows` is queryAll: include records in the recycle bin.
pub fn run(conn: &Connection, cat: &Catalog, ctx: &Ctx, soql: &str, all_rows: bool) -> SfResult<Output> {
    run_with_offset_limit(conn, cat, ctx, soql, all_rows, 2000)
}

/// `run` with a different OFFSET cap: the web UI pages through any number of records.
pub fn run_with_offset_limit(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    soql: &str,
    all_rows: bool,
    max_offset: u64,
) -> SfResult<Output> {
    let q = super::parse::parse(soql)?;
    run_parsed(conn, cat, ctx, soql, &q, all_rows, max_offset)
}

/// Run a query that's already parsed (and perhaps changed, as SOSL does). `soql` is for error messages.
pub fn run_parsed(
    conn: &Connection,
    cat: &Catalog,
    ctx: &Ctx,
    soql: &str,
    q: &Query,
    all_rows: bool,
    max_offset: u64,
) -> SfResult<Output> {
    let mut planner = Planner { cat, ctx, soql, all_rows, max_offset, params: vec![], next_alias: 0, grouping: None };
    let out = planner.top_level(conn, q)?;
    // FOR VIEW and FOR REFERENCE update the user's recently viewed records.
    if matches!(q.for_.as_deref(), Some("VIEW" | "REFERENCE")) {
        for r in &out.records {
            let url = r["attributes"]["url"].as_str().unwrap_or_default();
            if let Some((object, id)) = url.rsplit_once('/').and_then(|(a, id)| Some((a.rsplit_once('/')?.1, id))) {
                conn.execute(
                    "INSERT OR REPLACE INTO _recent (user, object, id, viewed) VALUES (?1, ?2, ?3, ?4)",
                    [&ctx.user_id, object, id, &ctx.now_str()],
                )?;
            }
        }
    }
    Ok(out)
}

/// The type a compiled expression has, for coercing literals and reading results.
#[derive(Clone)]
struct Typed {
    sql: String,
    field: Field,
}

struct Join {
    alias: String,
    /// The table alias and reference field this join hangs off.
    parent_alias: String,
    fk: String,
    /// The object joined to (`Name` for a polymorphic lookup), and its table.
    object: String,
    table: String,
    /// The relationship names from the base object, as written in the schema.
    path: Vec<String>,
}

struct Scope<'c> {
    obj: &'c SObject,
    alias: String,
    query_alias: Option<String>,
    joins: Vec<Join>,
}

/// A field shown by a TYPEOF branch: its name, type and result column.
type TypeOfField = (String, Field, usize);

enum Slot {
    /// A field value, nested under parent relationships.
    Field { rels: Vec<(String, usize)>, name: String, field: Box<Field>, col: usize },
    /// A child relationship subquery.
    Child { name: String, index: usize },
    /// `TYPEOF`: the lookup's value, and the fields to show for each type of record it can refer to.
    TypeOf {
        name: String,
        fk_col: usize,
        branches: Vec<(String, Vec<TypeOfField>)>,
        else_: Vec<(String, Field, usize)>,
    },
}

struct Planner<'a> {
    cat: &'a Catalog,
    ctx: &'a Ctx,
    soql: &'a str,
    all_rows: bool,
    max_offset: u64,
    params: Vec<Sql>,
    next_alias: usize,
    /// In an aggregate query: the grouping expressions and which are grouped in the current grouping set.
    grouping: Option<(Vec<String>, Vec<bool>)>,
}

fn is_aggregate(name: &str) -> bool {
    matches!(name, "COUNT" | "COUNT_DISTINCT" | "SUM" | "AVG" | "MIN" | "MAX")
}

fn has_aggregate(q: &Query) -> bool {
    !q.group_by.is_empty()
        || q.select
            .iter()
            .any(|s| matches!(s, SelectItem::Expr { expr: Expr::Func { name, .. }, .. } if is_aggregate(name)))
}

impl<'a> Planner<'a> {
    fn err(&self, code: &'static str, pos: usize, msg: impl AsRef<str>) -> SfError {
        query_error(code, self.soql, pos, msg.as_ref())
    }

    fn alias(&mut self) -> String {
        self.next_alias += 1;
        format!("t{}", self.next_alias)
    }

    fn param(&mut self, v: Sql) -> String {
        self.params.push(v);
        format!("?{}", self.params.len())
    }

    fn object(&self, name: &str, pos: usize) -> SfResult<&'a SObject> {
        self.cat.object(name).ok_or_else(|| {
            self.err(
                "INVALID_TYPE",
                pos,
                format!(
                    "sObject type '{name}' is not supported. If you are attempting to use a custom object, be sure to append the '__c' after the entity name. Please reference your WSDL or the describe call for the appropriate names."
                ),
            )
        })
    }

    fn scope(&mut self, obj: &'a SObject, query_alias: Option<String>) -> Scope<'a> {
        Scope { obj, alias: self.alias(), query_alias, joins: vec![] }
    }

    /// Resolve `Account.Owner.Name` to a column, adding LEFT JOINs for each parent hop.
    /// Returns the column SQL, the field, and the (relationship name, join index) hops.
    fn path(&mut self, sc: &mut Scope<'a>, parts: &[String], pos: usize) -> SfResult<(Typed, Vec<(String, usize)>)> {
        let mut parts = parts;
        if parts.len() > 1
            && let Some(a) = &sc.query_alias
            && parts[0].eq_ignore_ascii_case(a)
        {
            parts = &parts[1..];
        }
        let mut obj = sc.obj;
        let mut alias = sc.alias.clone();
        let mut hops = vec![];
        let mut path: Vec<String> = vec![];
        for rel in &parts[..parts.len() - 1] {
            let f = obj.relationship(rel).ok_or_else(|| {
                self.err(
                    "INVALID_FIELD",
                    pos,
                    format!(
                        "Didn't understand relationship '{rel}' in field path. If you are attempting to use a custom relationship, be sure to append the '__r' after the custom relationship name. Please reference your WSDL or the describe call for the appropriate names."
                    ),
                )
            })?;
            let rel_name = f.relationship_name.clone().unwrap();
            path.push(rel_name.clone());
            let parent = self.cat.parent_of(f).unwrap();
            let table = if f.is_polymorphic() { "_Name".to_string() } else { parent.name.clone() };
            let idx = match sc.joins.iter().position(|j| j.path == path) {
                Some(i) => i,
                None => {
                    let a = self.alias();
                    sc.joins.push(Join {
                        alias: a,
                        parent_alias: alias.clone(),
                        fk: f.name.clone(),
                        object: parent.name.clone(),
                        table,
                        path: path.clone(),
                    });
                    sc.joins.len() - 1
                }
            };
            hops.push((rel_name, idx));
            alias = sc.joins[idx].alias.clone();
            obj = parent;
        }
        let last = parts.last().unwrap();
        let f = obj.field(last).ok_or_else(|| {
            self.err(
                "INVALID_FIELD",
                pos,
                format!(
                    "No such column '{last}' on entity '{}'. If you are attempting to use a custom field, be sure to append the '__c' after the custom field name. Please reference your WSDL or the describe call for the appropriate names.",
                    obj.name
                ),
            )
        })?;
        let sql = if f.is_computed() {
            let env = Env { cat: self.cat, user_id: &self.ctx.user_id, now: self.ctx.now, prior: Prior::None };
            formula::field_sql_or_null(&env, obj, f, &alias)
        } else {
            format!("{alias}.{}", qi(&f.name))
        };
        Ok((Typed { sql, field: f.clone() }, hops))
    }

    /// Compile a value expression: a field path or a function over one.
    fn expr(&mut self, sc: &mut Scope<'a>, e: &Expr) -> SfResult<Typed> {
        match e {
            Expr::Path { parts, pos } => Ok(self.path(sc, parts, *pos)?.0),
            Expr::Func { name, args, pos } => {
                let arg = match args.first() {
                    Some(a) => Some(self.expr(sc, a)?),
                    None => None,
                };
                let synth = |ty: FieldType, name: &str| {
                    let mut f = arg
                        .as_ref()
                        .map(|a| a.field.clone())
                        .unwrap_or_else(|| self.cat.object("Account").unwrap().field("Id").unwrap().clone());
                    f.ty = ty;
                    f.name = name.into();
                    f
                };
                let a = || arg.as_ref().map(|a| a.sql.clone()).unwrap_or_default();
                if let ("SUM" | "AVG", Some(f)) = (name.as_str(), &arg)
                    && !matches!(
                        f.field.ty,
                        FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent
                    )
                {
                    return Err(self.err(
                        "MALFORMED_QUERY",
                        *pos,
                        format!("field {} does not support aggregate operator {name}", f.field.name),
                    ));
                }
                let (sql, field) = match name.as_str() {
                    "COUNT" if arg.is_none() => ("COUNT(*)".into(), synth(FieldType::Int, "expr")),
                    "COUNT" => (format!("COUNT({})", a()), synth(FieldType::Int, "expr")),
                    "COUNT_DISTINCT" => (format!("COUNT(DISTINCT {})", a()), synth(FieldType::Int, "expr")),
                    "SUM" | "AVG" => (format!("{name}({})", a()), synth(FieldType::Double, "expr")),
                    "MIN" | "MAX" => (format!("{name}({})", a()), arg.clone().unwrap().field),
                    "CALENDAR_YEAR" => (
                        format!("CAST(strftime('%Y', substr({}, 1, 10)) AS INTEGER)", a()),
                        synth(FieldType::Int, "expr"),
                    ),
                    "CALENDAR_MONTH" => (
                        format!("CAST(strftime('%m', substr({}, 1, 10)) AS INTEGER)", a()),
                        synth(FieldType::Int, "expr"),
                    ),
                    "CALENDAR_QUARTER" => (
                        format!("((CAST(strftime('%m', substr({}, 1, 10)) AS INTEGER) + 2) / 3)", a()),
                        synth(FieldType::Int, "expr"),
                    ),
                    "DAY_IN_MONTH" => (
                        format!("CAST(strftime('%d', substr({}, 1, 10)) AS INTEGER)", a()),
                        synth(FieldType::Int, "expr"),
                    ),
                    "HOUR_IN_DAY" => {
                        (format!("CAST(substr({}, 12, 2) AS INTEGER)", a()), synth(FieldType::Int, "expr"))
                    }
                    "DAY_ONLY" => (format!("substr({}, 1, 10)", a()), synth(FieldType::Date, "expr")),
                    "GROUPING" => {
                        let (groups, set) = self.grouping.as_ref().ok_or_else(|| {
                            self.err("MALFORMED_QUERY", *pos, "GROUPING can only be used with GROUP BY")
                        })?;
                        let k = groups.iter().position(|g| *g == a()).ok_or_else(|| {
                            self.err("MALFORMED_QUERY", *pos, "GROUPING's field must be in the GROUP BY clause")
                        })?;
                        ((if set[k] { "0" } else { "1" }).into(), synth(FieldType::Int, "expr"))
                    }
                    // Labels, formatting and currency conversion are identity functions here.
                    "TOLABEL" | "FORMAT" | "CONVERTCURRENCY" => {
                        let a = arg.clone().ok_or_else(|| self.err("MALFORMED_QUERY", *pos, "missing argument"))?;
                        (a.sql, a.field)
                    }
                    _ => return Err(self.err("MALFORMED_QUERY", *pos, format!("Unknown function: {name}"))),
                };
                Ok(Typed { sql, field })
            }
        }
    }

    fn sql_from(&self, sc: &Scope) -> String {
        let mut s = format!("{} {}", qi(&sc.obj.name), sc.alias);
        for j in &sc.joins {
            let a = &j.alias;
            s += &format!(" LEFT JOIN {} {a} ON {a}.\"Id\" = {}.{}", qi(&j.table), j.parent_alias, qi(&j.fk));
        }
        s
    }

    fn deleted_filter(&self, sc: &Scope) -> String {
        if self.all_rows { "1".into() } else { format!("{}.\"IsDeleted\" = 0", sc.alias) }
    }

    /// Coerce a SOQL literal to a value comparable with `f`.
    fn literal(&mut self, f: &Field, lit: &Literal, pos: usize) -> SfResult<Sql> {
        let wrong = |me: &Self, what: &str| {
            me.err(
                "INVALID_QUERY_FILTER_OPERATOR",
                pos,
                format!("value of filter criterion for field '{}' must be of type {what} and should not be enclosed in quotes", f.name),
            )
        };
        Ok(match (f.ty, lit) {
            (_, Literal::Null) => Sql::Null,
            (FieldType::Boolean, Literal::Bool(b)) => Sql::Integer(*b as i64),
            (FieldType::Boolean, _) => return Err(wrong(self, "boolean")),
            (FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent, Literal::Num(n)) => n
                .parse::<i64>()
                .map(Sql::Integer)
                .or_else(|_| n.parse::<f64>().map(Sql::Real))
                .map_err(|_| wrong(self, "number"))?,
            (FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent, _) => {
                return Err(wrong(self, "number"));
            }
            (FieldType::Date, Literal::Date(d)) => Sql::Text(d.clone()),
            (FieldType::Date, _) => return Err(wrong(self, "date")),
            (FieldType::Datetime, Literal::DateTime(d)) => {
                Sql::Text(format_datetime(parse_datetime(d).ok_or_else(|| wrong(self, "dateTime"))?))
            }
            (FieldType::Datetime, _) => return Err(wrong(self, "dateTime")),
            (FieldType::Id | FieldType::Reference, Literal::Str(s)) => match ids::to18(s) {
                Some(id) => Sql::Text(id),
                None => return Err(self.err("INVALID_QUERY_FILTER_OPERATOR", pos, format!("invalid ID field: {s}"))),
            },
            (_, Literal::Str(s)) => Sql::Text(s.replace("\\%", "%").replace("\\_", "_")),
            _ => {
                return Err(self.err(
                    "INVALID_QUERY_FILTER_OPERATOR",
                    pos,
                    format!(
                        "value of filter criterion for field '{}' must be of type string and should be enclosed in quotes",
                        f.name
                    ),
                ));
            }
        })
    }

    /// A field used where Salesforce doesn't allow it: `field 'X' can not be grouped in a query call`.
    fn check_use(&self, e: &Expr, t: &Typed, allowed: fn(&Field) -> bool, verb: &str) -> SfResult<()> {
        if matches!(e, Expr::Path { .. }) && !allowed(&t.field) {
            return Err(self.err(
                "INVALID_FIELD",
                e.pos(),
                format!("field '{}' can not be {verb} in a query call", t.field.name),
            ));
        }
        Ok(())
    }

    fn cond(&mut self, sc: &mut Scope<'a>, c: &Cond) -> SfResult<String> {
        Ok(match c {
            Cond::And(cs) => {
                let parts = cs.iter().map(|c| self.cond(sc, c)).collect::<SfResult<Vec<_>>>()?;
                format!("({})", parts.join(" AND "))
            }
            Cond::Or(cs) => {
                let parts = cs.iter().map(|c| self.cond(sc, c)).collect::<SfResult<Vec<_>>>()?;
                format!("({})", parts.join(" OR "))
            }
            // Every comparison below yields 0 or 1, never NULL, so NOT is
            // two-valued as it is in SOQL.
            Cond::Not(c) => format!("(NOT {})", self.cond(sc, c)?),
            Cond::Cmp { lhs, op, rhs, pos } => {
                let l = self.expr(sc, lhs)?;
                self.check_use(lhs, &l, Field::filterable, "filtered")?;
                if let Literal::DateLiteral(name, n) = rhs {
                    return self.date_literal(&l, op, name, *n, *pos);
                }
                if matches!(rhs, Literal::Null) {
                    return match *op {
                        "=" => Ok(format!("({} IS NULL)", l.sql)),
                        "!=" => Ok(format!("({} IS NOT NULL)", l.sql)),
                        _ => Err(self.err("MALFORMED_QUERY", *pos, "invalid operator on null")),
                    };
                }
                if *op == "LIKE" {
                    let Literal::Str(s) = rhs else {
                        return Err(self.err("MALFORMED_QUERY", *pos, "LIKE needs a string"));
                    };
                    let p = self.param(Sql::Text(s.clone()));
                    return Ok(format!("IFNULL({} LIKE {p} ESCAPE '\\', 0)", l.sql));
                }
                let v = self.literal(&l.field, rhs, *pos)?;
                let p = self.param(v);
                // SOQL's != matches records where the field is null; SQL's doesn't.
                let if_null = if *op == "!=" { 1 } else { 0 };
                format!("IFNULL({} {op} {p}, {if_null})", l.sql)
            }
            Cond::In { lhs, negated, source } => {
                let l = self.expr(sc, lhs)?;
                self.check_use(lhs, &l, Field::filterable, "filtered")?;
                let not = if *negated { "NOT " } else { "" };
                let if_null = *negated as i32;
                match source {
                    InSource::List(vals) => {
                        let ps = vals
                            .iter()
                            .map(|v| {
                                let s = self.literal(&l.field, v, lhs.pos())?;
                                Ok(self.param(s))
                            })
                            .collect::<SfResult<Vec<_>>>()?;
                        format!("IFNULL({} {not}IN ({}), {if_null})", l.sql, ps.join(", "))
                    }
                    InSource::Query(q) => {
                        let sub = self.semi_join(q)?;
                        format!("IFNULL({} {not}IN ({sub}), {if_null})", l.sql)
                    }
                }
            }
            Cond::Includes { lhs, excludes, values, pos } => {
                let l = self.expr(sc, lhs)?;
                if l.field.ty != FieldType::Multipicklist {
                    return Err(self.err(
                        "INVALID_QUERY_FILTER_OPERATOR",
                        *pos,
                        "includes or excludes operator only valid on multipicklist field",
                    ));
                }
                let any = values
                    .iter()
                    .map(|v| {
                        let all = v
                            .split(';')
                            .map(|part| {
                                let p = self.param(Sql::Text(format!(";{};", part.trim().to_lowercase())));
                                format!("instr(';' || lower({}) || ';', {p}) > 0", l.sql)
                            })
                            .collect::<Vec<_>>();
                        format!("({})", all.join(" AND "))
                    })
                    .collect::<Vec<_>>()
                    .join(" OR ");
                if *excludes { format!("IFNULL(NOT ({any}), 1)") } else { format!("IFNULL({any}, 0)") }
            }
        })
    }

    /// `Id IN (SELECT AccountId FROM Contact WHERE …)`
    fn semi_join(&mut self, q: &Query) -> SfResult<String> {
        let obj = self.object(&q.from, q.from_pos)?;
        let mut sc = self.scope(obj, q.alias.clone());
        let [SelectItem::Expr { expr, .. }] = q.select.as_slice() else {
            return Err(self.err("MALFORMED_QUERY", q.from_pos, "semi join sub-selects can only query a single field"));
        };
        let col = self.expr(&mut sc, expr)?;
        let mut wh = vec![self.deleted_filter(&sc), format!("{} IS NOT NULL", col.sql)];
        if let Some(c) = &q.where_ {
            wh.push(self.cond(&mut sc, c)?);
        }
        Ok(format!("SELECT {} FROM {} WHERE {}", col.sql, self.sql_from(&sc), wh.join(" AND ")))
    }

    fn date_literal(&mut self, l: &Typed, op: &str, name: &str, n: Option<i64>, pos: usize) -> SfResult<String> {
        let today = self.ctx.now.date_naive();
        let (start, end) = date_range(today, name, n)
            .ok_or_else(|| self.err("MALFORMED_QUERY", pos, format!("unexpected token: '{name}'")))?;
        let fmt = |d: NaiveDate| match l.field.ty {
            FieldType::Datetime => Sql::Text(format_datetime(d.and_hms_opt(0, 0, 0).unwrap().and_utc())),
            _ => Sql::Text(d.format("%Y-%m-%d").to_string()),
        };
        let (a, b) = (fmt(start), fmt(end));
        let c = &l.sql;
        Ok(match op {
            "=" => {
                let (a, b) = (self.param(a), self.param(b));
                format!("IFNULL({c} >= {a} AND {c} < {b}, 0)")
            }
            "!=" => {
                let (a, b) = (self.param(a), self.param(b));
                format!("IFNULL(NOT ({c} >= {a} AND {c} < {b}), 1)")
            }
            "<" => format!("IFNULL({c} < {}, 0)", self.param(a)),
            "<=" => format!("IFNULL({c} < {}, 0)", self.param(b)),
            ">" => format!("IFNULL({c} >= {}, 0)", self.param(b)),
            ">=" => format!("IFNULL({c} >= {}, 0)", self.param(a)),
            _ => return Err(self.err("MALFORMED_QUERY", pos, "invalid operator for a date literal")),
        })
    }

    fn top_level(&mut self, conn: &Connection, q: &Query) -> SfResult<Output> {
        if let Some(off) = q.offset
            && off > self.max_offset
        {
            return Err(SfError::new("NUMBER_OUTSIDE_VALID_RANGE", "Maximum SOQL offset allowed is 2000"));
        }
        let obj = self.object(&q.from, q.from_pos)?;
        if has_aggregate(q) {
            return self.aggregate(conn, obj, q);
        }
        let (records, _) = self.select(conn, obj, q, None)?;
        Ok(Output { total_size: records.len(), records })
    }

    /// Plain (non-aggregate) SELECT. With `parent_filter`, this is a child
    /// subquery: only rows whose `fk` is in the given ids, grouped by it.
    fn select(
        &mut self,
        conn: &Connection,
        obj: &'a SObject,
        q: &Query,
        parent_filter: Option<(&str, &[String])>,
    ) -> SfResult<(Vec<Value>, Vec<Option<String>>)> {
        let mut sc = self.scope(obj, q.alias.clone());
        let mut cols: Vec<String> = vec![format!("{}.\"Id\"", sc.alias)];
        let mut slots = vec![];
        let mut subs = vec![];

        let mut seen = std::collections::HashSet::new();
        for item in &q.select {
            if let SelectItem::Expr { expr: Expr::Path { parts, pos }, alias: None } = item
                && !seen.insert(parts.join(".").to_ascii_lowercase())
            {
                return Err(self.err("INVALID_FIELD", *pos, format!("duplicate field selected: {}", parts.join("."))));
            }
        }

        let mut items: Vec<SelectItem> = vec![];
        for item in &q.select {
            match item {
                SelectItem::Fields { kind, pos } => {
                    if kind != "STANDARD" && q.limit.is_none_or(|l| l > 200) {
                        return Err(self.err(
                            "MALFORMED_QUERY",
                            *pos,
                            "The SOQL FIELDS function must have a LIMIT of at most 200",
                        ));
                    }
                    for f in &obj.fields {
                        let keep = match kind.as_str() {
                            "ALL" => true,
                            "CUSTOM" => f.custom,
                            "STANDARD" => !f.custom,
                            _ => return Err(self.err("MALFORMED_QUERY", *pos, format!("unexpected token: '{kind}'"))),
                        };
                        if keep {
                            items.push(SelectItem::Expr {
                                expr: Expr::Path { parts: vec![f.name.clone()], pos: *pos },
                                alias: None,
                            });
                        }
                    }
                }
                other => items.push(other.clone()),
            }
        }

        for item in &items {
            match item {
                SelectItem::Expr { expr, .. } => {
                    let (typed, rels) = match expr {
                        Expr::Path { parts, pos } => self.path(&mut sc, parts, *pos)?,
                        Expr::Func { .. } => (self.expr(&mut sc, expr)?, vec![]),
                    };
                    let name = typed.field.name.clone();
                    // Each parent hop needs its Id, for `attributes.url` and to tell a null parent apart.
                    for (_, j) in &rels {
                        let c = format!("{}.\"Id\"", sc.joins[*j].alias);
                        if !cols.contains(&c) {
                            cols.push(c);
                        }
                    }
                    cols.push(typed.sql.clone());
                    slots.push(Slot::Field { rels, name, field: Box::new(typed.field), col: cols.len() - 1 });
                }
                SelectItem::Sub(sub) => {
                    let rel = self.cat.child_relationship(&obj.name, &sub.from).ok_or_else(|| {
                        self.err(
                            "INVALID_TYPE",
                            sub.from_pos,
                            format!(
                                "Didn't understand relationship '{}' in FROM part of query call. If you are attempting to use a custom relationship, be sure to append the '__r' after the custom relationship name. Please reference your WSDL or the describe call for the appropriate names.",
                                sub.from
                            ),
                        )
                    })?;
                    slots.push(Slot::Child { name: rel.name.clone(), index: subs.len() });
                    subs.push((rel.clone(), sub));
                }
                SelectItem::TypeOf { rel, pos, whens, else_ } => {
                    slots.push(self.type_of(&mut sc, &mut cols, rel, *pos, whens, else_)?);
                }
                SelectItem::Fields { .. } => unreachable!(),
            }
        }

        let mut wh = vec![self.deleted_filter(&sc)];
        if let Some(c) = &q.where_ {
            wh.push(self.cond(&mut sc, c)?);
        }
        let mut fk_col = None;
        if let Some((fk, parent_ids)) = parent_filter {
            let ps: Vec<String> = parent_ids.iter().map(|id| self.param(Sql::Text(id.clone()))).collect();
            let c = format!("{}.{}", sc.alias, qi(fk));
            wh.push(format!("{c} IN ({})", ps.join(", ")));
            cols.push(c);
            fk_col = Some(cols.len() - 1);
        }
        let order = self.order_by(&mut sc, q)?;
        let mut sql =
            format!("SELECT {} FROM {} WHERE {}{order}", cols.join(", "), self.sql_from(&sc), wh.join(" AND "));
        // In a subquery, LIMIT applies per parent: applied below instead.
        if parent_filter.is_none() {
            sql += &limit_clause(q);
        }

        let rows = query_rows(conn, &sql, &self.params, cols.len())?;

        // Child subqueries, fetched in one query per relationship for all the parents.
        let parent_ids: Vec<String> = rows.iter().filter_map(|r| text(&r[0])).collect();
        let mut child_results: Vec<HashMap<String, Vec<Value>>> = vec![];
        for (rel, sub) in &subs {
            let child = self.cat.object(&rel.child_object).unwrap();
            let mut grouped: HashMap<String, Vec<Value>> = HashMap::new();
            for chunk in parent_ids.chunks(500) {
                let mut inner = Planner {
                    cat: self.cat,
                    ctx: self.ctx,
                    soql: self.soql,
                    all_rows: self.all_rows,
                    max_offset: self.max_offset,
                    params: vec![],
                    next_alias: 0,
                    grouping: None,
                };
                let (recs, fks) = inner.select(conn, child, sub, Some((&rel.field, chunk)))?;
                for (r, fk) in recs.into_iter().zip(fks) {
                    let list = grouped.entry(fk.unwrap_or_default()).or_default();
                    if sub.limit.is_none_or(|l| (list.len() as u64) < l) {
                        list.push(r);
                    }
                }
            }
            child_results.push(grouped);
        }

        let mut out = vec![];
        let mut fks = vec![];
        for row in rows {
            let id = text(&row[0]);
            let mut rec = Map::new();
            rec.insert("attributes".into(), attributes(self.ctx, &obj.name, id.as_deref()));
            for slot in &slots {
                match slot {
                    Slot::Field { rels, name, field, col } => {
                        put_nested(self.ctx, self.cat, &sc, &cols, &row, &mut rec, rels, name, field, *col)
                    }
                    Slot::TypeOf { name, fk_col, branches, else_ } => {
                        let v = match text(&row[*fk_col]) {
                            None => Value::Null,
                            Some(id) => {
                                let ty = self.cat.object_for_id(&id).map(|o| o.name.clone()).unwrap_or_default();
                                let fields = branches.iter().find(|(t, _)| *t == ty).map(|(_, f)| f).unwrap_or(else_);
                                let mut m = Map::new();
                                m.insert("attributes".into(), attributes(self.ctx, &ty, Some(&id)));
                                for (n, f, c) in fields {
                                    m.insert(n.clone(), from_sql(f, row[*c].clone()));
                                }
                                Value::Object(m)
                            }
                        };
                        rec.insert(name.clone(), v);
                    }
                    Slot::Child { name, index } => {
                        let recs =
                            id.as_ref().and_then(|id| child_results[*index].get(id)).cloned().unwrap_or_default();
                        let v = if recs.is_empty() {
                            Value::Null
                        } else {
                            json!({ "totalSize": recs.len(), "done": true, "records": recs })
                        };
                        rec.insert(name.clone(), v);
                    }
                }
            }
            fks.push(fk_col.and_then(|c| text(&row[c])));
            out.push(Value::Object(rec));
        }
        Ok((out, fks))
    }

    /// Compile `TYPEOF rel WHEN Type THEN fields … ELSE fields END`: one join per type named.
    fn type_of(
        &mut self,
        sc: &mut Scope<'a>,
        cols: &mut Vec<String>,
        rel: &str,
        pos: usize,
        whens: &[(String, usize, Vec<Expr>)],
        else_: &[Expr],
    ) -> SfResult<Slot> {
        let f = sc
            .obj
            .relationship(rel)
            .filter(|f| f.is_polymorphic())
            .ok_or_else(|| {
                self.err(
                    "INVALID_FIELD",
                    pos,
                    format!("TYPEOF can only be used with a polymorphic relationship, not {rel}"),
                )
            })?
            .clone();
        cols.push(format!("{}.{}", sc.alias, qi(&f.name)));
        let fk_col = cols.len() - 1;
        let fields_of =
            |me: &mut Self, sc: &mut Scope<'a>, cols: &mut Vec<String>, exprs: &[Expr], prefix: &[String]| {
                let mut out = vec![];
                for e in exprs {
                    let Expr::Path { parts, pos } = e else {
                        return Err(me.err("MALFORMED_QUERY", e.pos(), "TYPEOF can only select fields"));
                    };
                    if parts.len() != 1 {
                        return Err(me.err("MALFORMED_QUERY", *pos, "TYPEOF can't follow relationships"));
                    }
                    let full: Vec<String> = prefix.iter().chain(parts).cloned().collect();
                    let (t, _) = me.path(sc, &full, *pos)?;
                    cols.push(t.sql);
                    out.push((t.field.name.clone(), t.field, cols.len() - 1));
                }
                Ok(out)
            };
        let mut branches = vec![];
        for (ty, ty_pos, exprs) in whens {
            let target =
                f.targets().find(|t| t.eq_ignore_ascii_case(ty)).and_then(|t| self.cat.object(t)).ok_or_else(|| {
                    self.err("INVALID_FIELD", *ty_pos, format!("{ty} isn't one of the types {rel} can refer to"))
                })?;
            // A join of its own: the target's table, matched on the lookup's id.
            let alias = self.alias();
            sc.joins.push(Join {
                alias: alias.clone(),
                parent_alias: sc.alias.clone(),
                fk: f.name.clone(),
                object: target.name.clone(),
                table: target.name.clone(),
                path: vec![format!("{rel}#{}", target.name)],
            });
            let mut inner = Scope { obj: target, alias, query_alias: None, joins: vec![] };
            let fields = fields_of(self, &mut inner, cols, exprs, &[])?;
            branches.push((target.name.clone(), fields));
        }
        let else_fields = fields_of(self, sc, cols, else_, &[rel.to_string()])?;
        Ok(Slot::TypeOf { name: f.relationship_name.clone().unwrap(), fk_col, branches, else_: else_fields })
    }

    fn order_by(&mut self, sc: &mut Scope<'a>, q: &Query) -> SfResult<String> {
        if q.order_by.is_empty() {
            return Ok(if has_aggregate(q) { String::new() } else { format!(" ORDER BY {}.rowid", sc.alias) });
        }
        let mut parts = vec![];
        for o in &q.order_by {
            let e = self.expr(sc, &o.expr)?;
            self.check_use(&o.expr, &e, Field::sortable, "sorted")?;
            let dir = if o.desc { "DESC" } else { "ASC" };
            // Salesforce sorts nulls first unless told otherwise, in either direction.
            let nulls = if o.nulls_last == Some(true) { "NULLS LAST" } else { "NULLS FIRST" };
            parts.push(format!("{} {dir} {nulls}", e.sql));
        }
        Ok(format!(" ORDER BY {}", parts.join(", ")))
    }

    fn aggregate(&mut self, conn: &Connection, obj: &'a SObject, q: &Query) -> SfResult<Output> {
        let mut sc = self.scope(obj, q.alias.clone());
        // SELECT COUNT() FROM … returns only totalSize.
        if let [SelectItem::Expr { expr: Expr::Func { name, args, .. }, .. }] = q.select.as_slice()
            && name == "COUNT"
            && args.is_empty()
            && q.group_by.is_empty()
        {
            let mut wh = vec![self.deleted_filter(&sc)];
            if let Some(c) = &q.where_ {
                wh.push(self.cond(&mut sc, c)?);
            }
            let sql = format!("SELECT COUNT(*) FROM {} WHERE {}", self.sql_from(&sc), wh.join(" AND "));
            let mut n = match &query_rows(conn, &sql, &self.params, 1)?[0][0] {
                Sql::Integer(n) => *n as usize,
                _ => 0,
            };
            if let Some(l) = q.limit {
                n = n.min(l as usize);
            }
            return Ok(Output { total_size: n, records: vec![] });
        }

        // GROUP BY ROLLUP and CUBE run one grouping set per subtotal level, unioned.
        let n = q.group_by.len();
        let sets: Vec<Vec<bool>> = match q.group_mode {
            GroupMode::Plain => vec![vec![true; n]],
            GroupMode::Rollup => (0..=n).rev().map(|k| (0..n).map(|i| i < k).collect()).collect(),
            GroupMode::Cube => {
                (0..1usize << n).rev().map(|bits| (0..n).map(|i| bits & (1 << (n - 1 - i)) != 0).collect()).collect()
            }
        };
        if q.group_mode != GroupMode::Plain && n > 3 {
            return Err(self.err("MALFORMED_QUERY", q.from_pos, "ROLLUP and CUBE can group by at most 3 fields"));
        }
        let mut keys = vec![];
        let mut item_sql = vec![];
        let mut subs = vec![];
        for (si, set) in sets.iter().enumerate() {
            let mut group_sql = vec![];
            for g in &q.group_by {
                let t = self.expr(&mut sc, g)?;
                self.check_use(g, &t, Field::groupable, "grouped")?;
                group_sql.push(t.sql);
            }
            self.grouping = Some((group_sql.clone(), set.clone()));
            let mut cols = vec![];
            let mut expr_n = 0;
            for (i, item) in q.select.iter().enumerate() {
                let SelectItem::Expr { expr, alias } = item else {
                    return Err(self.err(
                        "MALFORMED_QUERY",
                        q.from_pos,
                        "subqueries and FIELDS() can't be used with aggregate functions",
                    ));
                };
                let t = self.expr(&mut sc, expr)?;
                let grouped = group_sql.iter().position(|g| *g == t.sql);
                if matches!(expr, Expr::Path { .. }) && grouped.is_none() {
                    return Err(self.err(
                        "MALFORMED_QUERY",
                        expr.pos(),
                        format!("Field must be grouped or aggregated: {}", t.field.name),
                    ));
                }
                let col = match grouped {
                    Some(k) if !set[k] => "NULL".to_string(),
                    _ => t.sql.clone(),
                };
                cols.push(format!("{col} AS c{i}"));
                if si == 0 {
                    let key = match (alias, expr) {
                        (Some(a), _) => a.clone(),
                        (None, Expr::Path { .. }) => t.field.name.clone(),
                        (None, Expr::Func { .. }) => {
                            expr_n += 1;
                            format!("expr{}", expr_n - 1)
                        }
                    };
                    keys.push((key, t.field));
                    item_sql.push(t.sql);
                }
            }
            for (k, g) in group_sql.iter().enumerate() {
                cols.push(format!("{} AS lf_g{k}", (!set[k]) as i32));
                cols.push(format!("{} AS lf_v{k}", if set[k] { g.as_str() } else { "NULL" }));
            }
            let mut wh = vec![self.deleted_filter(&sc)];
            if let Some(c) = &q.where_ {
                wh.push(self.cond(&mut sc, c)?);
            }
            let mut sql = format!("SELECT {} FROM {{FROM}} WHERE {}", cols.join(", "), wh.join(" AND "));
            let grouped: Vec<&str> =
                group_sql.iter().zip(set).filter(|(_, on)| **on).map(|(g, _)| g.as_str()).collect();
            if !grouped.is_empty() {
                sql += &format!(" GROUP BY {}", grouped.join(", "));
            }
            if let Some(h) = &q.having {
                sql += &format!(" HAVING {}", self.cond(&mut sc, h)?);
            }
            subs.push(sql);
        }

        let order = if q.group_mode == GroupMode::Plain {
            String::new()
        } else if q.order_by.is_empty() {
            // Subtotals after the rows they total, the grand total last.
            let parts: Vec<String> = (0..n).map(|k| format!("lf_g{k}, lf_v{k}")).collect();
            format!(" ORDER BY {}", parts.join(", "))
        } else {
            let mut parts = vec![];
            for o in &q.order_by {
                let e = self.expr(&mut sc, &o.expr)?;
                let i = item_sql.iter().position(|s| *s == e.sql).ok_or_else(|| {
                    self.err(
                        "MALFORMED_QUERY",
                        o.expr.pos(),
                        "with ROLLUP or CUBE, ORDER BY can only use selected fields",
                    )
                })?;
                let dir = if o.desc { "DESC" } else { "ASC" };
                let nulls = if o.nulls_last == Some(true) { "NULLS LAST" } else { "NULLS FIRST" };
                parts.push(format!("c{i} {dir} {nulls}"));
            }
            format!(" ORDER BY {}", parts.join(", "))
        };
        let sql = if q.group_mode == GroupMode::Plain {
            // One grouping set: ORDER BY can use anything grouped or aggregated, selected or not.
            let order = self.order_by(&mut sc, q)?;
            format!("{}{order}{}", subs[0], limit_clause(q))
        } else {
            let outer: Vec<String> = (0..keys.len()).map(|i| format!("c{i}")).collect();
            format!("SELECT {} FROM ({}){order}{}", outer.join(", "), subs.join(" UNION ALL "), limit_clause(q))
        };
        self.grouping = None;
        let sql = sql.replace("{FROM}", &self.sql_from(&sc));

        let rows = query_rows(conn, &sql, &self.params, keys.len())?;
        let records: Vec<Value> = rows
            .into_iter()
            .map(|row| {
                let mut rec = Map::new();
                rec.insert("attributes".into(), json!({ "type": "AggregateResult" }));
                for ((key, f), v) in keys.iter().zip(row) {
                    rec.insert(key.clone(), from_sql(f, v));
                }
                Value::Object(rec)
            })
            .collect();
        Ok(Output { total_size: records.len(), records })
    }
}

fn limit_clause(q: &Query) -> String {
    match (q.limit, q.offset) {
        (Some(l), Some(o)) => format!(" LIMIT {l} OFFSET {o}"),
        (Some(l), None) => format!(" LIMIT {l}"),
        (None, Some(o)) => format!(" LIMIT -1 OFFSET {o}"),
        (None, None) => String::new(),
    }
}

fn text(v: &Sql) -> Option<String> {
    match v {
        Sql::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn query_rows(conn: &Connection, sql: &str, params: &[Sql], width: usize) -> SfResult<Vec<Vec<Sql>>> {
    tracing::debug!(%sql, "soql");
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |r| (0..width).map(|i| r.get::<_, Sql>(i)).collect())?
        .collect::<Result<Vec<Vec<Sql>>, _>>()?;
    Ok(rows)
}

/// Put a field into the record, creating `{"attributes": …}` objects for each
/// parent hop, or a null where the lookup is empty.
#[allow(clippy::too_many_arguments)]
fn put_nested(
    ctx: &Ctx,
    cat: &Catalog,
    sc: &Scope,
    cols: &[String],
    row: &[Sql],
    rec: &mut Map<String, Value>,
    rels: &[(String, usize)],
    name: &str,
    field: &Field,
    col: usize,
) {
    let mut target = rec;
    for (rel, j) in rels {
        let join = &sc.joins[*j];
        let id_col = cols.iter().position(|c| *c == format!("{}.\"Id\"", join.alias)).unwrap();
        let Some(id) = text(&row[id_col]) else {
            target.entry(rel.clone()).or_insert(Value::Null);
            return;
        };
        let entry = target.entry(rel.clone()).or_insert_with(|| {
            let mut m = Map::new();
            let attrs = if join.table == "_Name" {
                // A polymorphic lookup's parent is a `Name`, at its own object's URL.
                let actual = cat.object_for_id(&id).map(|o| o.name.as_str()).unwrap_or("Name");
                json!({ "type": "Name", "url": crate::store::record_url(ctx, actual, &id) })
            } else {
                attributes(ctx, &join.object, Some(&id))
            };
            m.insert("attributes".into(), attrs);
            Value::Object(m)
        });
        match entry {
            Value::Object(m) => target = m,
            _ => return,
        }
    }
    target.insert(name.to_string(), from_sql(field, row[col].clone()));
}

/// The [start, end) dates a date literal such as `LAST_N_DAYS:30` covers.
pub fn date_range(today: NaiveDate, name: &str, n: Option<i64>) -> Option<(NaiveDate, NaiveDate)> {
    let day = Duration::days(1);
    let week_start = today - Duration::days(today.weekday().num_days_from_sunday() as i64);
    let month_start = today.with_day(1)?;
    let q_month = (today.month0() / 3) * 3 + 1;
    let quarter_start = NaiveDate::from_ymd_opt(today.year(), q_month, 1)?;
    let year_start = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
    let add_months = |d: NaiveDate, m: i64| {
        if m >= 0 {
            d.checked_add_months(Months::new(m as u32))
        } else {
            d.checked_sub_months(Months::new((-m) as u32))
        }
    };
    let n = n.unwrap_or(0);
    Some(match name {
        "TODAY" => (today, today + day),
        "YESTERDAY" => (today - day, today),
        "TOMORROW" => (today + day, today + day * 2),
        "THIS_WEEK" => (week_start, week_start + Duration::days(7)),
        "LAST_WEEK" => (week_start - Duration::days(7), week_start),
        "NEXT_WEEK" => (week_start + Duration::days(7), week_start + Duration::days(14)),
        "THIS_MONTH" => (month_start, add_months(month_start, 1)?),
        "LAST_MONTH" => (add_months(month_start, -1)?, month_start),
        "NEXT_MONTH" => (add_months(month_start, 1)?, add_months(month_start, 2)?),
        "THIS_QUARTER" => (quarter_start, add_months(quarter_start, 3)?),
        "LAST_QUARTER" => (add_months(quarter_start, -3)?, quarter_start),
        "NEXT_QUARTER" => (add_months(quarter_start, 3)?, add_months(quarter_start, 6)?),
        "THIS_YEAR" => (year_start, add_months(year_start, 12)?),
        "LAST_YEAR" => (add_months(year_start, -12)?, year_start),
        "NEXT_YEAR" => (add_months(year_start, 12)?, add_months(year_start, 24)?),
        "LAST_90_DAYS" => (today - Duration::days(90), today + day),
        "NEXT_90_DAYS" => (today + day, today + Duration::days(91)),
        "LAST_N_DAYS" => (today - Duration::days(n), today + day),
        "NEXT_N_DAYS" => (today + day, today + Duration::days(n + 1)),
        "N_DAYS_AGO" => (today - Duration::days(n), today - Duration::days(n - 1)),
        "LAST_N_WEEKS" => (week_start - Duration::days(7 * n), week_start),
        "NEXT_N_WEEKS" => (week_start + Duration::days(7), week_start + Duration::days(7 * (n + 1))),
        "LAST_N_MONTHS" => (add_months(month_start, -n)?, month_start),
        "NEXT_N_MONTHS" => (add_months(month_start, 1)?, add_months(month_start, n + 1)?),
        "LAST_N_YEARS" => (add_months(year_start, -12 * n)?, year_start),
        "NEXT_N_YEARS" => (add_months(year_start, 12)?, add_months(year_start, 12 * (n + 1))?),
        _ => return None,
    })
}
