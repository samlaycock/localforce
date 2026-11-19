//! Salesforce formulas, compiled to SQLite expressions.
//!
//! Formula fields, roll-up summaries and validation rules all compile to
//! SQL over a record's row, so a formula field can be selected, filtered and
//! sorted on in SOQL like any other column, and is never stale. Parent
//! fields (`Account__r.Name`) become correlated subqueries.
//!
//! The language: literals, field references (with parent relationship paths,
//! `$User`, `$Organization`, `$Profile` and `$System`), `+ - * / ^ &`,
//! comparisons, `&& || !`, and Salesforce's functions: logical, text, math,
//! date and time, `VLOOKUP`, `DISTANCE` and `GEOLOCATION`. Text comparisons
//! are case-sensitive and blank text is the empty string, as in Salesforce.
//!
//! Default values are formulas too: `default_value` evaluates one, with no
//! record to read.

use std::cell::Cell;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as Sql;

use crate::schema::{Catalog, Derived, Field, FieldType, Rollup, SObject};
use crate::store::{format_datetime, qi};

/// What a validation rule can see of the record's earlier state.
#[derive(Clone, Copy)]
pub enum Prior<'a> {
    /// A formula field: there's no "before".
    None,
    /// The record is being created: `ISNEW()` is true.
    New,
    /// The record is being updated: its stored values before the change.
    Old(&'a HashMap<String, Sql>),
}

pub struct Env<'a> {
    pub cat: &'a Catalog,
    pub user_id: &'a str,
    pub now: DateTime<Utc>,
    pub prior: Prior<'a>,
}

/// The SQL for a formula or roll-up summary field of `obj`, reading the row at `alias`.
pub fn field_sql(env: &Env, obj: &SObject, f: &Field, alias: &str) -> Result<String, String> {
    Compiler::new(env).derived_field(obj, f, alias)
}

/// Like `field_sql`, but a formula that doesn't compile reads as null (and is logged).
pub fn field_sql_or_null(env: &Env, obj: &SObject, f: &Field, alias: &str) -> String {
    field_sql(env, obj, f, alias).unwrap_or_else(|e| {
        tracing::debug!("{}.{}: {e}", obj.name, f.name);
        "NULL".into()
    })
}

/// A validation rule's error condition, as SQL that's 1 when the record is invalid.
pub fn condition_sql(env: &Env, obj: &SObject, src: &str, alias: &str) -> Result<String, String> {
    let ast = parse(src)?;
    let out = Compiler::new(env).compile(obj, alias, &ast, true)?;
    if !matches!(out.kind, Kind::Bool | Kind::Null) {
        return Err("a validation rule's formula must return true or false".into());
    }
    Ok(format!("IFNULL(({}), 0)", out.sql))
}

/// Compile every formula and validation rule, returning what doesn't compile.
pub fn check(cat: &Catalog) -> Vec<String> {
    let env = Env { cat, user_id: "", now: Utc::now(), prior: Prior::None };
    let mut problems = vec![];
    for o in &cat.objects {
        for f in &o.fields {
            if matches!(f.derived, Derived::Formula(_) | Derived::Rollup(_))
                && let Err(e) = field_sql(&env, o, f, "r")
            {
                problems.push(format!("formula {}.{}: {e} (it reads as null)", o.name, f.name));
            }
        }
        for f in &o.fields {
            if let Some(src) = &f.default_formula
                && let Err(e) = check_default(cat, o, f, src)
            {
                problems.push(format!("default value of {}.{}: {e} (it's ignored)", o.name, f.name));
            }
        }
        for r in &o.validation_rules {
            let env = Env { cat, user_id: "", now: Utc::now(), prior: Prior::New };
            if let Err(e) = condition_sql(&env, o, &r.condition, "r") {
                problems.push(format!("validation rule {}.{}: {e} (it's ignored)", o.name, r.name));
            }
        }
    }
    problems
}

/// Check one formula, for the web UI's field editor.
pub fn check_formula(cat: &Catalog, obj: &SObject, src: &str) -> Result<(), String> {
    let env = Env { cat, user_id: "", now: Utc::now(), prior: Prior::None };
    Compiler::new(&env).compile(obj, "r", &parse(src)?, true).map(|_| ())
}

/// The SQL for a field's default-value formula, which can't read the record being created.
fn default_sql(env: &Env, obj: &SObject, f: &Field, src: &str) -> Result<String, String> {
    let c = Compiler { no_record: true, ..Compiler::new(env) };
    let out = c.compile(obj, "r", &parse(src)?, true)?;
    if out.kind == Kind::Geo {
        return Err("GEOLOCATION can only be used in DISTANCE".into());
    }
    Ok(c.shape(out, f))
}

/// Check a default-value formula, for loading and the web UI's field editor.
pub fn check_default(cat: &Catalog, obj: &SObject, f: &Field, src: &str) -> Result<(), String> {
    let env = Env { cat, user_id: "", now: Utc::now(), prior: Prior::New };
    default_sql(&env, obj, f, src).map(|_| ())
}

/// Evaluate a field's default-value formula for a record being created.
pub fn default_value(conn: &Connection, env: &Env, obj: &SObject, f: &Field, src: &str) -> Result<Sql, String> {
    let sql = default_sql(env, obj, f, src)?;
    conn.query_row(&format!("SELECT {sql}"), [], |r| r.get::<_, Sql>(0)).map_err(|e| e.to_string())
}

/// SQL functions formulas compile to that SQLite doesn't have built in.
pub fn register_functions(conn: &Connection) -> rusqlite::Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    let math = |name: &str, f: fn(f64) -> f64| {
        conn.create_scalar_function(name, 1, flags, move |ctx| {
            Ok(ctx.get::<Option<f64>>(0)?.map(f).filter(|v| v.is_finite()))
        })
    };
    math("lf_floor", f64::floor)?;
    math("lf_ceil", f64::ceil)?;
    math("lf_trunc", f64::trunc)?;
    // CEILING rounds away from zero: CEILING(-2.5) is -3.
    math("lf_ceil_away", |x| x.signum() * x.abs().ceil())?;
    math("lf_sqrt", f64::sqrt)?;
    math("lf_exp", f64::exp)?;
    math("lf_ln", f64::ln)?;
    math("lf_log10", f64::log10)?;
    conn.create_scalar_function("lf_pow", 2, flags, |ctx| {
        let (a, b) = (ctx.get::<Option<f64>>(0)?, ctx.get::<Option<f64>>(1)?);
        Ok(a.zip(b).map(|(a, b)| a.powf(b)).filter(|v| v.is_finite()))
    })?;
    // ROUND(n, digits), where digits can be negative: ROUND(1250, -2) is 1300.
    conn.create_scalar_function("lf_round", 2, flags, |ctx| {
        let (Some(x), Some(d)) = (ctx.get::<Option<f64>>(0)?, ctx.get::<Option<i64>>(1)?) else { return Ok(None) };
        let m = 10f64.powi(-d as i32);
        Ok(Some((x / m).abs().round().copysign(x) * m).filter(|v| v.is_finite()))
    })?;
    let text = |name: &str, f: fn(&str) -> String| {
        conn.create_scalar_function(name, 1, flags, move |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|s| f(&s))))
    };
    text("lf_htmlencode", html_encode)?;
    text("lf_jsencode", js_encode)?;
    text("lf_urlencode", url_encode)?;
    text("lf_initcap", initcap)?;
    text("lf_reverse", |s| s.chars().rev().collect())?;
    text("lf_casesafeid", |s| crate::ids::to18(s).unwrap_or_else(|| s.to_string()))?;
    text("lf_timevalue", |s| {
        let t = s.trim().trim_end_matches('Z');
        chrono::NaiveTime::parse_from_str(t, "%H:%M:%S%.f")
            .or_else(|_| chrono::NaiveTime::parse_from_str(t, "%H:%M"))
            .map(|t| t.format("%H:%M:%S%.3fZ").to_string())
            .unwrap_or_default()
    })?;
    conn.create_scalar_function("lf_isnumber", 1, flags, |ctx| {
        // Not locale-aware, and any space makes it false: "1 000" and " 12 " aren't numbers.
        let s = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        let s = s.as_str();
        let digits = s.trim_start_matches(['+', '-']);
        let ok = !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
            && digits.starts_with(|c: char| c.is_ascii_digit() || c == '.')
            && s.parse::<f64>().is_ok_and(f64::is_finite);
        Ok(ok as i64)
    })?;
    // ISOWEEK and ISOYEAR of a date.
    let iso = |name: &str, f: fn(chrono::IsoWeek) -> i64| {
        conn.create_scalar_function(name, 1, flags, move |ctx| {
            use chrono::Datelike;
            Ok(ctx
                .get::<Option<String>>(0)?
                .and_then(|d| chrono::NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok())
                .map(|d| f(d.iso_week())))
        })
    };
    iso("lf_isoweek", |w| w.week() as i64)?;
    iso("lf_isoyear", |w| w.year() as i64)?;
    // A time plus milliseconds wraps around midnight.
    conn.create_scalar_function("lf_time_add", 2, flags, |ctx| {
        let (Some(t), Some(ms)) = (ctx.get::<Option<String>>(0)?, ctx.get::<Option<f64>>(1)?) else {
            return Ok(None);
        };
        Ok(time_ms(&t).map(|base| {
            let day = 86_400_000i64;
            let n = (base + ms as i64).rem_euclid(day);
            format!("{:02}:{:02}:{:02}.{:03}Z", n / 3_600_000, n / 60_000 % 60, n / 1000 % 60, n % 1000)
        }))
    })?;
    conn.create_scalar_function("lf_time_ms", 1, flags, |ctx| {
        Ok(ctx.get::<Option<String>>(0)?.and_then(|t| time_ms(&t)))
    })?;
    // DISTANCE(GEOLOCATION(lat, lon), GEOLOCATION(lat, lon), unit): great-circle distance.
    conn.create_scalar_function("lf_distance", 5, flags, |ctx| {
        let mut v = [0f64; 4];
        for (i, x) in v.iter_mut().enumerate() {
            let Some(n) = ctx.get::<Option<f64>>(i)? else { return Ok(None) };
            *x = n.to_radians();
        }
        let radius = match ctx.get::<Option<String>>(4)?.as_deref() {
            Some("km") => 6371.0088,
            _ => 3958.7613,
        };
        let h = ((v[2] - v[0]) / 2.0).sin().powi(2) + v[0].cos() * v[2].cos() * ((v[3] - v[1]) / 2.0).sin().powi(2);
        Ok(Some(2.0 * radius * h.sqrt().asin()))
    })?;
    // REGEX(text, pattern): the whole text must match.
    conn.create_scalar_function("lf_regex", 2, flags, |ctx| {
        let re = ctx.get_or_create_aux(1, |v| -> Result<regex::Regex, Box<dyn std::error::Error + Send + Sync>> {
            Ok(regex::Regex::new(&format!("^(?:{})$", v.as_str()?))?)
        })?;
        let text = ctx.get::<Option<String>>(0)?.unwrap_or_default();
        Ok(re.is_match(&text) as i64)
    })?;
    // ADDMONTHS(date, n): the last day of a month stays the last day.
    conn.create_scalar_function("lf_addmonths", 2, flags, |ctx| {
        use chrono::{Datelike, Months, NaiveDate};
        let (Some(d), Some(n)) = (ctx.get::<Option<String>>(0)?, ctx.get::<Option<i64>>(1)?) else {
            return Ok(None);
        };
        let Ok(date) = NaiveDate::parse_from_str(d.get(..10).unwrap_or(&d), "%Y-%m-%d") else { return Ok(None) };
        let last_day = date.succ_opt().is_some_and(|next| next.month() != date.month());
        let moved = if n >= 0 {
            date.checked_add_months(Months::new(n as u32))
        } else {
            date.checked_sub_months(Months::new(n.unsigned_abs() as u32))
        };
        let moved = moved.map(|m| {
            if last_day {
                let first_next = m.with_day(1).and_then(|f| f.checked_add_months(Months::new(1)));
                first_next.and_then(|f| f.pred_opt()).unwrap_or(m)
            } else {
                m
            }
        });
        Ok(moved.map(|m| format!("{}{}", m.format("%Y-%m-%d"), d.get(10..).unwrap_or(""))))
    })?;
    Ok(())
}

/// Milliseconds since midnight of a time (`13:45:00.000Z`).
fn time_ms(t: &str) -> Option<i64> {
    let t = chrono::NaiveTime::parse_from_str(t.trim_end_matches('Z'), "%H:%M:%S%.f").ok()?;
    use chrono::Timelike;
    Some(t.num_seconds_from_midnight() as i64 * 1000 + (t.nanosecond() / 1_000_000) as i64)
}

/// HTMLENCODE: the characters HTML reserves, as entities.
fn html_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// JSENCODE: backslashes before the characters that would end or break a JavaScript string.
fn js_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '/' => out.push_str("\\/"),
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            c => out.push(c),
        }
    }
    out
}

/// URLENCODE: everything but RFC 3986's unreserved characters, percent-encoded as UTF-8.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// INITCAP: each word capitalised, the rest lowercase.
fn initcap(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start {
            out.extend(c.to_uppercase());
        } else {
            out.extend(c.to_lowercase());
        }
        start = c.is_whitespace();
    }
    out
}

// Parsing

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(String),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let end = src[i + 2..].find("*/").ok_or("unterminated comment")?;
            i += end + 4;
            continue;
        }
        let two = src.get(i..i + 2).unwrap_or("");
        let op2 = ["==", "!=", "<>", "<=", ">=", "&&", "||"].into_iter().find(|o| *o == two);
        if let Some(o) = op2 {
            out.push(Tok::Op(if o == "<>" { "!=" } else { o }));
            i += 2;
            continue;
        }
        let tok = match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b',' => Tok::Comma,
            b'+' => Tok::Op("+"),
            b'-' => Tok::Op("-"),
            b'*' => Tok::Op("*"),
            b'/' => Tok::Op("/"),
            b'^' => Tok::Op("^"),
            b'&' => Tok::Op("&"),
            b'=' => Tok::Op("="),
            b'<' => Tok::Op("<"),
            b'>' => Tok::Op(">"),
            b'!' => Tok::Op("!"),
            b'"' | b'\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                loop {
                    let Some(&ch) = b.get(i) else { return Err("unterminated text literal".into()) };
                    if ch == quote {
                        break;
                    }
                    if ch == b'\\' {
                        let e = *b.get(i + 1).ok_or("unterminated text literal")?;
                        s.push(match e {
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            other => other as char,
                        });
                        i += 2;
                        continue;
                    }
                    let chr = src[i..].chars().next().unwrap();
                    s.push(chr);
                    i += chr.len_utf8();
                }
                out.push(Tok::Str(s));
                i += 1;
                continue;
            }
            c if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                out.push(Tok::Num(src[start..i].to_string()));
                continue;
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$' | b'.')) {
                    i += 1;
                }
                out.push(Tok::Ident(src[start..i].to_string()));
                continue;
            }
            other => return Err(format!("unexpected character '{}'", other as char)),
        };
        out.push(tok);
        i += 1;
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum Ast {
    Num(String),
    Str(String),
    Bool(bool),
    Null,
    Ref(Vec<String>),
    Call(String, Vec<Ast>),
    Neg(Box<Ast>),
    Not(Box<Ast>),
    Bin(&'static str, Box<Ast>, Box<Ast>),
}

fn parse(src: &str) -> Result<Ast, String> {
    let mut p = Parser { toks: lex(src)?, i: 0 };
    if p.toks.is_empty() {
        return Err("the formula is empty".into());
    }
    let ast = p.or()?;
    if p.i < p.toks.len() {
        return Err(format!("unexpected {}", p.shown()));
    }
    Ok(ast)
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn shown(&self) -> String {
        match self.peek() {
            None => "end of formula".into(),
            Some(Tok::Num(s) | Tok::Ident(s)) => format!("'{s}'"),
            Some(Tok::Str(s)) => format!("\"{s}\""),
            Some(Tok::Op(o)) => format!("'{o}'"),
            Some(Tok::LParen) => "'('".into(),
            Some(Tok::RParen) => "')'".into(),
            Some(Tok::Comma) => "','".into(),
        }
    }
    fn eat_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Op(o)) if ops.contains(o) => {
                let o = *o;
                self.i += 1;
                Some(o)
            }
            _ => None,
        }
    }
    fn binary(&mut self, ops: &[&'static str], next: fn(&mut Self) -> Result<Ast, String>) -> Result<Ast, String> {
        let mut lhs = next(self)?;
        while let Some(op) = self.eat_op(ops) {
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(next(self)?));
        }
        Ok(lhs)
    }
    fn or(&mut self) -> Result<Ast, String> {
        self.binary(&["||"], Self::and)
    }
    fn and(&mut self) -> Result<Ast, String> {
        self.binary(&["&&"], Self::cmp)
    }
    fn cmp(&mut self) -> Result<Ast, String> {
        self.binary(&["=", "==", "!=", "<", "<=", ">", ">="], Self::add)
    }
    fn add(&mut self) -> Result<Ast, String> {
        self.binary(&["+", "-", "&"], Self::mul)
    }
    fn mul(&mut self) -> Result<Ast, String> {
        self.binary(&["*", "/"], Self::unary)
    }
    fn unary(&mut self) -> Result<Ast, String> {
        if self.eat_op(&["-"]).is_some() {
            return Ok(Ast::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op(&["+"]).is_some() {
            return self.unary();
        }
        if self.eat_op(&["!"]).is_some() {
            return Ok(Ast::Not(Box::new(self.unary()?)));
        }
        self.pow()
    }
    fn pow(&mut self) -> Result<Ast, String> {
        let base = self.atom()?;
        if self.eat_op(&["^"]).is_some() {
            return Ok(Ast::Bin("^", Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }
    fn atom(&mut self) -> Result<Ast, String> {
        let Some(t) = self.peek().cloned() else { return Err("the formula ends too soon".into()) };
        self.i += 1;
        Ok(match t {
            Tok::Num(n) => Ast::Num(n),
            Tok::Str(s) => Ast::Str(s),
            Tok::LParen => {
                let e = self.or()?;
                if self.peek() != Some(&Tok::RParen) {
                    return Err(format!("expected ')' but found {}", self.shown()));
                }
                self.i += 1;
                e
            }
            Tok::Ident(name) if self.peek() == Some(&Tok::LParen) => {
                self.i += 1;
                let mut args = vec![];
                if self.peek() != Some(&Tok::RParen) {
                    loop {
                        args.push(self.or()?);
                        match self.peek() {
                            Some(Tok::Comma) => self.i += 1,
                            Some(Tok::RParen) => break,
                            _ => return Err(format!("expected ',' or ')' but found {}", self.shown())),
                        }
                    }
                }
                self.i += 1;
                Ast::Call(name.to_ascii_uppercase(), args)
            }
            Tok::Ident(name) => match name.to_ascii_uppercase().as_str() {
                "TRUE" => Ast::Bool(true),
                "FALSE" => Ast::Bool(false),
                "NULL" => Ast::Null,
                _ => Ast::Ref(name.split('.').map(String::from).collect()),
            },
            _ => {
                self.i -= 1;
                return Err(format!("unexpected {}", self.shown()));
            }
        })
    }
}

// Compiling

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Num,
    Text,
    Bool,
    Date,
    DateTime,
    /// A time of day: `13:45:00.000Z`.
    Time,
    /// `GEOLOCATION(lat, lon)`, as the SQL `lat, lon`: only `DISTANCE` takes one.
    Geo,
    /// `NULL`, which fits anywhere.
    Null,
}

fn kind_of(ty: FieldType) -> Kind {
    match ty {
        FieldType::Int | FieldType::Double | FieldType::Currency | FieldType::Percent => Kind::Num,
        FieldType::Boolean => Kind::Bool,
        FieldType::Date => Kind::Date,
        FieldType::Datetime => Kind::DateTime,
        _ => Kind::Text,
    }
}

struct C {
    sql: String,
    kind: Kind,
    /// For a field reference: the value without blank handling, for ISBLANK and friends.
    raw: Option<String>,
}

impl C {
    fn new(sql: impl Into<String>, kind: Kind) -> C {
        C { sql: sql.into(), kind, raw: None }
    }
    fn raw(&self) -> &str {
        self.raw.as_deref().unwrap_or(&self.sql)
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sql_literal(v: &Sql) -> String {
    match v {
        Sql::Null | Sql::Blob(_) => "NULL".into(),
        Sql::Integer(i) => i.to_string(),
        Sql::Real(r) => format!("{r:?}"),
        Sql::Text(s) => quote(s),
    }
}

/// `julianday` of a stored datetime (`2024-01-31T10:00:00.000+0000`, always UTC).
fn jd(sql: &str) -> String {
    format!("julianday(substr({sql}, 1, 23))")
}

fn datetime_from_jd(jd: &str) -> String {
    format!("(strftime('%Y-%m-%dT%H:%M:%f', {jd}) || '+0000')")
}

/// A number as Salesforce's TEXT() shows it: no trailing `.0`.
fn num_text(sql: &str) -> String {
    format!(
        "(CASE WHEN {sql} = CAST({sql} AS INTEGER) THEN CAST(CAST({sql} AS INTEGER) AS TEXT) ELSE CAST({sql} AS TEXT) END)"
    )
}

struct Compiler<'a> {
    env: &'a Env<'a>,
    aliases: Cell<usize>,
    depth: Cell<usize>,
    /// A default value: there's no record, so fields can't be read.
    no_record: bool,
}

impl<'a> Compiler<'a> {
    fn new(env: &'a Env<'a>) -> Self {
        Compiler { env, aliases: Cell::new(0), depth: Cell::new(0), no_record: false }
    }

    fn alias(&self) -> String {
        self.aliases.set(self.aliases.get() + 1);
        format!("lf_f{}", self.aliases.get())
    }

    /// A formula or roll-up field, shaped like its result type.
    fn derived_field(&self, obj: &SObject, f: &Field, alias: &str) -> Result<String, String> {
        if self.depth.get() > 10 {
            return Err("formulas refer to each other in a loop".into());
        }
        self.depth.set(self.depth.get() + 1);
        let out = match &f.derived {
            Derived::Formula(src) => {
                let ast = parse(src)?;
                let c = self.compile(obj, alias, &ast, f.blank_as_zero)?;
                if c.kind == Kind::Geo {
                    return Err("GEOLOCATION can only be used in DISTANCE".into());
                }
                Ok(self.shape(c, f))
            }
            Derived::Rollup(r) => self.rollup(obj, r, alias),
            Derived::Address(p) => Ok(address_sql(p, alias)),
            _ => Err("not a formula".into()),
        };
        self.depth.set(self.depth.get() - 1);
        out
    }

    /// Convert a compiled value to what a field of `f`'s type holds.
    fn shape(&self, c: C, f: &Field) -> String {
        let s = c.sql;
        match (f.ty, c.kind) {
            (FieldType::Boolean, _) => format!("IFNULL(({s}), 0)"),
            (FieldType::Int, _) => format!("CAST(round({s}) AS INTEGER)"),
            (FieldType::Double | FieldType::Currency | FieldType::Percent, _) => {
                format!("round({s}, {})", f.scale.unwrap_or(2))
            }
            (FieldType::Date, Kind::DateTime) => format!("substr({s}, 1, 10)"),
            (FieldType::Date | FieldType::Datetime, _) => s,
            (_, Kind::Num) => format!("NULLIF({}, '')", num_text(&s)),
            (_, Kind::Bool) => format!("(CASE WHEN {s} THEN 'true' ELSE 'false' END)"),
            _ => format!("NULLIF(({s}), '')"),
        }
    }

    fn rollup(&self, obj: &SObject, r: &Rollup, alias: &str) -> Result<String, String> {
        let child = self.env.cat.object(&r.object).ok_or_else(|| format!("unknown object {}", r.object))?;
        let fk = child.field(&r.via).ok_or_else(|| format!("unknown field {}.{}", child.name, r.via))?;
        if fk.reference_to.as_deref().is_none_or(|t| !t.eq_ignore_ascii_case(&obj.name)) {
            return Err(format!("{}.{} doesn't refer to {}", child.name, fk.name, obj.name));
        }
        let a = self.alias();
        let value = match (&r.field, r.op.as_str()) {
            (_, "count") => "COUNT(*)".to_string(),
            (Some(field), op @ ("sum" | "min" | "max")) => {
                let f = child.field(field).ok_or_else(|| format!("unknown field {}.{field}", child.name))?;
                let col = self.column(child, f, &a)?;
                format!("{}({col})", op.to_ascii_uppercase())
            }
            (None, op) => return Err(format!("a {op} roll-up needs a field")),
            (_, op) => return Err(format!("unknown roll-up operation {op}")),
        };
        let mut wh = vec![format!("{a}.{} = {alias}.\"Id\"", qi(&fk.name)), format!("{a}.\"IsDeleted\" = 0")];
        for flt in &r.filters {
            let f = child.field(&flt.field).ok_or_else(|| format!("unknown field {}.{}", child.name, flt.field))?;
            let col = self.column(child, f, &a)?;
            let values: Vec<String> = flt.value.split(',').map(|v| self.filter_value(f, v.trim())).collect();
            let any = |op: &str| values.iter().map(|v| format!("{col} {op} {v}")).collect::<Vec<_>>().join(" OR ");
            wh.push(match flt.operation.as_str() {
                "equals" => format!("IFNULL(({}), 0)", any("=")),
                "notEqual" => format!("NOT IFNULL(({}), 0)", any("=")),
                "lessThan" => format!("IFNULL({col} < {}, 0)", values[0]),
                "greaterThan" => format!("IFNULL({col} > {}, 0)", values[0]),
                "lessOrEqual" => format!("IFNULL({col} <= {}, 0)", values[0]),
                "greaterOrEqual" => format!("IFNULL({col} >= {}, 0)", values[0]),
                "contains" => format!("IFNULL(instr(lower({col}), lower({})) > 0, 0)", values[0]),
                "startsWith" => format!("IFNULL(instr(lower({col}), lower({})) = 1, 0)", values[0]),
                other => return Err(format!("roll-up filter operation {other} isn't supported")),
            });
        }
        let sql = format!("(SELECT {value} FROM {} {a} WHERE {})", qi(&child.name), wh.join(" AND "));
        Ok(if r.op == "count" {
            sql
        } else if r.op == "sum" {
            format!("IFNULL({sql}, 0)")
        } else {
            sql
        })
    }

    fn filter_value(&self, f: &Field, v: &str) -> String {
        match kind_of(f.ty) {
            Kind::Bool => (if v.eq_ignore_ascii_case("true") { "1" } else { "0" }).into(),
            Kind::Num if v.parse::<f64>().is_ok() => v.into(),
            _ => quote(v),
        }
    }

    /// A field's stored value (or computed value, for formula fields).
    fn column(&self, obj: &SObject, f: &Field, alias: &str) -> Result<String, String> {
        match f.derived {
            Derived::Formula(_) | Derived::Rollup(_) | Derived::Address(_) => self.derived_field(obj, f, alias),
            _ => Ok(format!("{alias}.{}", qi(&f.name))),
        }
    }

    /// A field reference: `Budget__c`, `Account__r.Owner.Name`, `$User.Id`.
    fn reference(&self, obj: &SObject, alias: &str, parts: &[String], blank_as_zero: bool) -> Result<C, String> {
        let (raw, f) = if parts[0].eq_ignore_ascii_case("$User") && parts.len() == 2 {
            let user = self.env.cat.object("User").unwrap();
            let f = user.field(&parts[1]).ok_or_else(|| format!("unknown field $User.{}", parts[1]))?;
            let a = self.alias();
            let col = self.column(user, f, &a)?;
            (format!("(SELECT {col} FROM \"User\" {a} WHERE {a}.\"Id\" = {})", quote(self.env.user_id)), f.clone())
        } else if parts[0].starts_with('$') {
            return self.global(parts);
        } else {
            if self.no_record {
                return Err(format!("a default value can't refer to the record's fields ({})", parts.join(".")));
            }
            self.path(obj, alias, parts)?
        };
        let kind = kind_of(f.ty);
        let sql = match kind {
            Kind::Num if blank_as_zero => format!("IFNULL({raw}, 0)"),
            Kind::Text => format!("IFNULL({raw}, '')"),
            Kind::Bool => format!("IFNULL({raw}, 0)"),
            _ => raw.clone(),
        };
        Ok(C { sql, kind, raw: Some(raw) })
    }

    /// `$Organization.Id`, `$Profile.Name`, `$System.OriginDateTime`.
    fn global(&self, parts: &[String]) -> Result<C, String> {
        let name = parts.join(".");
        let upper = name.to_ascii_uppercase();
        let text = |v: &str| Ok(C::new(quote(v), Kind::Text));
        match upper.as_str() {
            "$ORGANIZATION.ID" => text(&crate::ids::make("00D", crate::state::ORG_SEQ)),
            "$ORGANIZATION.NAME" => text("localforce"),
            "$PROFILE.NAME" => text("System Administrator"),
            "$SYSTEM.ORIGINDATETIME" => Ok(C::new(quote("1900-01-01T00:00:00.000+0000"), Kind::DateTime)),
            "$API.SESSION_ID" => text(""),
            _ => Err(format!("{name} isn't supported")),
        }
    }

    fn path(&self, obj: &SObject, alias: &str, parts: &[String]) -> Result<(String, Field), String> {
        if parts.len() == 1 {
            let f = obj.field(&parts[0]).ok_or_else(|| format!("Field {} does not exist", parts[0]))?;
            return Ok((self.column(obj, f, alias)?, f.clone()));
        }
        let rel =
            obj.relationship(&parts[0]).ok_or_else(|| format!("Field {} does not exist. Check spelling.", parts[0]))?;
        if !rel.also_to.is_empty() {
            return Err(format!("{} is polymorphic: formulas can't follow it", parts[0]));
        }
        let parent = self.env.cat.object(rel.reference_to.as_deref().unwrap()).unwrap();
        let a = self.alias();
        let (inner, f) = self.path(parent, &a, &parts[1..])?;
        let sql =
            format!("(SELECT {inner} FROM {} {a} WHERE {a}.\"Id\" = {alias}.{})", qi(&parent.name), qi(&rel.name));
        Ok((sql, f))
    }

    fn compile(&self, obj: &SObject, alias: &str, ast: &Ast, blank_as_zero: bool) -> Result<C, String> {
        let go = |a: &Ast| self.compile(obj, alias, a, blank_as_zero);
        Ok(match ast {
            Ast::Num(n) => {
                n.parse::<f64>().map_err(|_| format!("invalid number {n}"))?;
                C::new(if n.contains('.') { n.clone() } else { format!("{n}.0") }, Kind::Num)
            }
            Ast::Str(s) => C::new(quote(s), Kind::Text),
            Ast::Bool(b) => C::new(if *b { "1" } else { "0" }, Kind::Bool),
            Ast::Null => C::new("NULL", Kind::Null),
            Ast::Ref(parts) => self.reference(obj, alias, parts, blank_as_zero)?,
            Ast::Neg(a) => {
                let a = go(a)?;
                expect(&a, &[Kind::Num], "-")?;
                C::new(format!("(-{})", a.sql), Kind::Num)
            }
            Ast::Not(a) => {
                let a = go(a)?;
                C::new(format!("(NOT IFNULL({}, 0))", a.sql), Kind::Bool)
            }
            Ast::Bin(op, a, b) => self.binary(op, go(a)?, go(b)?)?,
            Ast::Call(name, args) => self.call(obj, alias, name, args, blank_as_zero)?,
        })
    }

    fn binary(&self, op: &str, a: C, b: C) -> Result<C, String> {
        use Kind::*;
        let (x, y) = (&a.sql, &b.sql);
        Ok(match op {
            "&" => C::new(format!("({} || {})", text_of(&a), text_of(&b)), Text),
            "+" => match (a.kind, b.kind) {
                (Text, _) | (_, Text) => C::new(format!("({x} || {y})"), Text),
                (Date, Num) | (Num, Date) => {
                    let (d, n) = if a.kind == Date { (x, y) } else { (y, x) };
                    C::new(format!("date({d}, printf('%+d days', CAST({n} AS INTEGER)))"), Date)
                }
                (DateTime, Num) | (Num, DateTime) => {
                    let (d, n) = if a.kind == DateTime { (x, y) } else { (y, x) };
                    C::new(datetime_from_jd(&format!("({} + {n})", jd(d))), DateTime)
                }
                // A time plus a number of milliseconds.
                (Time, Num) | (Num, Time) => {
                    let (t, n) = if a.kind == Time { (x, y) } else { (y, x) };
                    C::new(format!("lf_time_add({t}, {n})"), Time)
                }
                _ => {
                    expect(&a, &[Num], "+")?;
                    expect(&b, &[Num], "+")?;
                    C::new(format!("({x} + {y})"), Num)
                }
            },
            "-" => match (a.kind, b.kind) {
                (Date, Date) => C::new(format!("CAST(julianday({x}) - julianday({y}) AS INTEGER)"), Num),
                (DateTime, DateTime) => C::new(format!("({} - {})", jd(x), jd(y)), Num),
                (Date, Num) => C::new(format!("date({x}, printf('%+d days', -CAST({y} AS INTEGER)))"), Date),
                (DateTime, Num) => C::new(datetime_from_jd(&format!("({} - {y})", jd(x))), DateTime),
                // Milliseconds from the second time to the first, wrapping at midnight: never negative.
                (Time, Time) => {
                    C::new(format!("(((lf_time_ms({x}) - lf_time_ms({y})) % 86400000 + 86400000) % 86400000)"), Num)
                }
                (Time, Num) => C::new(format!("lf_time_add({x}, -({y}))"), Time),
                _ => {
                    expect(&a, &[Num], "-")?;
                    expect(&b, &[Num], "-")?;
                    C::new(format!("({x} - {y})"), Num)
                }
            },
            "*" | "/" | "^" => {
                expect(&a, &[Num], op)?;
                expect(&b, &[Num], op)?;
                match op {
                    "*" => C::new(format!("({x} * {y})"), Num),
                    "/" => C::new(format!("({x} * 1.0 / NULLIF({y}, 0))"), Num),
                    _ => C::new(format!("lf_pow({x}, {y})"), Num),
                }
            }
            "=" | "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                if a.kind != b.kind && ![a.kind, b.kind].contains(&Null) {
                    return Err(format!("Incorrect parameter type for operator '{op}'"));
                }
                let op = if op == "==" { "=" } else { op };
                let (x, y) = match a.kind {
                    DateTime => (jd(x), jd(y)),
                    _ => (x.clone(), y.clone()),
                };
                C::new(format!("({x} {op} {y})"), Bool)
            }
            "&&" => C::new(format!("(IFNULL({x}, 0) AND IFNULL({y}, 0))"), Bool),
            "||" => C::new(format!("(IFNULL({x}, 0) OR IFNULL({y}, 0))"), Bool),
            _ => return Err(format!("unknown operator {op}")),
        })
    }

    fn call(&self, obj: &SObject, alias: &str, name: &str, args: &[Ast], blank_as_zero: bool) -> Result<C, String> {
        use Kind::*;
        let n_args = |range: std::ops::RangeInclusive<usize>| {
            if range.contains(&args.len()) {
                Ok(())
            } else {
                Err(format!(
                    "Incorrect number of parameters for function '{name}()'. Expected {}, received {}",
                    if range.start() == range.end() {
                        range.start().to_string()
                    } else {
                        format!("{}-{}", range.start(), range.end())
                    },
                    args.len()
                ))
            }
        };
        // ISBLANK and friends look at the stored value, not the blank-as-zero one.
        let raw_args = matches!(name, "ISBLANK" | "ISNULL" | "BLANKVALUE" | "NULLVALUE");
        let compiled: Vec<C> = if matches!(name, "ISCHANGED" | "PRIORVALUE" | "VLOOKUP") {
            vec![]
        } else {
            args.iter().map(|a| self.compile(obj, alias, a, blank_as_zero && !raw_args)).collect::<Result<_, _>>()?
        };
        if compiled.iter().any(|c| c.kind == Geo) && !matches!(name, "DISTANCE") {
            return Err(format!("GEOLOCATION can't be used in {name}(): only in DISTANCE()"));
        }
        let a = |i: usize| compiled[i].sql.clone();
        let first_kind = |cs: &[C]| cs.iter().map(|c| c.kind).find(|k| *k != Null).unwrap_or(Null);
        Ok(match name {
            "IF" => {
                n_args(3..=3)?;
                let kind = first_kind(&compiled[1..]);
                C::new(format!("(CASE WHEN IFNULL({}, 0) THEN {} ELSE {} END)", a(0), a(1), a(2)), kind)
            }
            "AND" | "OR" => {
                n_args(1..=usize::MAX)?;
                let parts: Vec<String> = compiled.iter().map(|c| format!("IFNULL({}, 0)", c.sql)).collect();
                C::new(format!("({})", parts.join(&format!(" {name} "))), Bool)
            }
            "NOT" => {
                n_args(1..=1)?;
                C::new(format!("(NOT IFNULL({}, 0))", a(0)), Bool)
            }
            "CASE" => {
                if args.len() < 4 || !args.len().is_multiple_of(2) {
                    return Err("Incorrect number of parameters for function 'CASE()'".into());
                }
                let mut s = format!("(CASE {}", compiled[0].sql);
                for pair in compiled[1..compiled.len() - 1].chunks(2) {
                    s += &format!(" WHEN {} THEN {}", pair[0].sql, pair[1].sql);
                }
                s += &format!(" ELSE {} END)", compiled.last().unwrap().sql);
                let results: Vec<C> = compiled[1..]
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| i % 2 == 1 || *i == compiled.len() - 2)
                    .map(|(_, c)| C::new(c.sql.clone(), c.kind))
                    .collect();
                C::new(s, first_kind(&results))
            }
            "ISBLANK" | "ISNULL" => {
                n_args(1..=1)?;
                let r = compiled[0].raw();
                if compiled[0].kind == Text {
                    C::new(format!("(IFNULL({r}, '') = '')"), Bool)
                } else {
                    C::new(format!("({r} IS NULL)"), Bool)
                }
            }
            "BLANKVALUE" | "NULLVALUE" => {
                n_args(2..=2)?;
                let r = compiled[0].raw();
                let v = if compiled[0].kind == Text { format!("NULLIF({r}, '')") } else { r.to_string() };
                C::new(format!("COALESCE({v}, {})", a(1)), first_kind(&compiled))
            }
            "ISPICKVAL" => {
                n_args(2..=2)?;
                C::new(format!("(IFNULL({}, '') = {})", compiled[0].raw(), a(1)), Bool)
            }
            "INCLUDES" => {
                n_args(2..=2)?;
                C::new(
                    format!("(instr(';' || IFNULL({}, '') || ';', ';' || {} || ';') > 0)", compiled[0].raw(), a(1)),
                    Bool,
                )
            }
            "ISNEW" => {
                n_args(0..=0)?;
                C::new(if matches!(self.env.prior, Prior::New) { "1" } else { "0" }, Bool)
            }
            "ISCHANGED" | "PRIORVALUE" => {
                n_args(1..=1)?;
                let Ast::Ref(parts) = &args[0] else { return Err(format!("{name}() needs a field")) };
                let [field] = parts.as_slice() else { return Err(format!("{name}() needs a field on this object")) };
                let f = obj.field(field).ok_or_else(|| format!("Field {field} does not exist"))?;
                let now = self.column(obj, f, alias)?;
                match (&self.env.prior, name) {
                    (Prior::Old(old), "ISCHANGED") => {
                        let before = old.get(&f.name).map(sql_literal).unwrap_or_else(|| "NULL".into());
                        C::new(format!("({now} IS NOT {before})"), Bool)
                    }
                    (_, "ISCHANGED") => C::new("0", Bool),
                    (Prior::Old(old), _) => {
                        C::new(old.get(&f.name).map(sql_literal).unwrap_or_else(|| "NULL".into()), kind_of(f.ty))
                    }
                    (_, _) => C::new(now, kind_of(f.ty)),
                }
            }
            "TEXT" => {
                n_args(1..=1)?;
                let c = &compiled[0];
                let s = match c.kind {
                    Num => num_text(&c.sql),
                    DateTime => format!("(replace(substr({}, 1, 19), 'T', ' ') || 'Z')", c.sql),
                    Bool => format!("(CASE WHEN {} THEN 'true' ELSE 'false' END)", c.sql),
                    _ => format!("IFNULL({}, '')", c.raw()),
                };
                C::new(s, Text)
            }
            "VALUE" => {
                n_args(1..=1)?;
                C::new(format!("CAST(NULLIF(trim({}), '') AS REAL)", a(0)), Num)
            }
            "LEN" => {
                n_args(1..=1)?;
                C::new(format!("length(IFNULL({}, ''))", a(0)), Num)
            }
            // UPPER and LOWER take an optional locale, which makes no difference here.
            "UPPER" | "LOWER" => {
                n_args(1..=2)?;
                C::new(format!("{}({})", name.to_ascii_lowercase(), a(0)), Text)
            }
            "TRIM" => {
                n_args(1..=1)?;
                C::new(format!("trim({}, ' ' || char(9))", a(0)), Text)
            }
            "INITCAP" | "REVERSE" | "HTMLENCODE" | "JSENCODE" | "URLENCODE" | "CASESAFEID" => {
                n_args(1..=1)?;
                C::new(format!("lf_{}({})", name.to_ascii_lowercase(), a(0)), Text)
            }
            "JSINHTMLENCODE" => {
                n_args(1..=1)?;
                C::new(format!("lf_jsencode(lf_htmlencode({}))", a(0)), Text)
            }
            "ASCII" => {
                n_args(1..=1)?;
                C::new(format!("(CASE WHEN IFNULL({0}, '') = '' THEN NULL ELSE unicode({0}) END)", a(0)), Num)
            }
            "CHR" => {
                n_args(1..=1)?;
                C::new(format!("char(CAST({} AS INTEGER))", a(0)), Text)
            }
            "BR" => {
                n_args(0..=0)?;
                C::new("'<br>'", Text)
            }
            "GETSESSIONID" => {
                n_args(0..=0)?;
                C::new("''", Text)
            }
            "IMAGE" => {
                n_args(2..=4)?;
                let size = if args.len() == 4 {
                    format!(
                        " || '\" height=\"' || {} || '\" width=\"' || {}",
                        text_of(&compiled[2]),
                        text_of(&compiled[3])
                    )
                } else {
                    String::new()
                };
                C::new(
                    format!("('<img src=\"' || {} || '\" alt=\"' || {}{size} || '\" border=\"0\"/>')", a(0), a(1)),
                    Text,
                )
            }
            "ISNUMBER" => {
                n_args(1..=1)?;
                C::new(format!("lf_isnumber({})", a(0)), Bool)
            }
            "ISCLONE" => {
                n_args(0..=0)?;
                C::new("0", Bool)
            }
            "PICKLISTCOUNT" => {
                n_args(1..=1)?;
                C::new(
                    format!(
                        "(CASE WHEN IFNULL({0}, '') = '' THEN 0 ELSE length({0}) - length(replace({0}, ';', '')) + 1 END)",
                        compiled[0].raw()
                    ),
                    Num,
                )
            }
            "CURRENCYRATE" => {
                n_args(1..=1)?;
                C::new("1.0", Num)
            }
            "VLOOKUP" => {
                n_args(3..=3)?;
                let object_field = |a: &Ast| -> Result<(&SObject, Field), String> {
                    let Ast::Ref(p) = a else { return Err("VLOOKUP() needs $ObjectType.Object.Fields.Field".into()) };
                    match p.as_slice() {
                        [ot, o, fs, f]
                            if ot.eq_ignore_ascii_case("$ObjectType") && fs.eq_ignore_ascii_case("Fields") =>
                        {
                            let o = self.env.cat.object(o).ok_or_else(|| format!("unknown object {o}"))?;
                            let f = o.field(f).ok_or_else(|| format!("Field {f} does not exist"))?.clone();
                            Ok((o, f))
                        }
                        _ => Err("VLOOKUP() needs $ObjectType.Object.Fields.Field".into()),
                    }
                };
                let (o, ret) = object_field(&args[0])?;
                let (o2, key) = object_field(&args[1])?;
                if o.name != o2.name {
                    return Err("VLOOKUP()'s fields must be on the same object".into());
                }
                let value = self.compile(obj, alias, &args[2], blank_as_zero)?;
                let la = self.alias();
                let col = self.column(o, &ret, &la)?;
                let sql = format!(
                    "(SELECT {col} FROM {} {la} WHERE {la}.\"IsDeleted\" = 0 AND {la}.{} = {} LIMIT 1)",
                    qi(&o.name),
                    qi(&key.name),
                    value.sql
                );
                let kind = kind_of(ret.ty);
                let out = if kind == Text { format!("IFNULL({sql}, '')") } else { sql.clone() };
                C { sql: out, kind, raw: Some(sql) }
            }
            "LEFT" => {
                n_args(2..=2)?;
                C::new(format!("substr({}, 1, MAX(CAST({} AS INTEGER), 0))", a(0), a(1)), Text)
            }
            "RIGHT" => {
                n_args(2..=2)?;
                C::new(
                    format!("(CASE WHEN {1} <= 0 THEN '' ELSE substr({0}, -CAST({1} AS INTEGER)) END)", a(0), a(1)),
                    Text,
                )
            }
            "MID" => {
                n_args(3..=3)?;
                C::new(format!("substr({}, MAX(CAST({} AS INTEGER), 1), CAST({} AS INTEGER))", a(0), a(1), a(2)), Text)
            }
            "LPAD" | "RPAD" => {
                n_args(2..=3)?;
                let pad = if args.len() == 3 { a(2) } else { "' '".into() };
                let n = format!("MAX(CAST({} AS INTEGER), 0)", a(1));
                let fill = format!("replace(hex(zeroblob({n})), '00', {pad})");
                let s = if name == "LPAD" {
                    format!(
                        "(CASE WHEN length({0}) >= {n} THEN substr({0}, 1, {n}) ELSE substr({fill} || {0}, -{n}) END)",
                        a(0)
                    )
                } else {
                    format!("substr({} || {fill}, 1, {n})", a(0))
                };
                C::new(s, Text)
            }
            "CONTAINS" => {
                n_args(2..=2)?;
                C::new(format!("(instr({}, {}) > 0)", a(0), a(1)), Bool)
            }
            "BEGINS" => {
                n_args(2..=2)?;
                C::new(format!("(instr({}, {}) = 1 OR {} = '')", a(0), a(1), a(1)), Bool)
            }
            "FIND" => {
                n_args(2..=3)?;
                if args.len() == 3 {
                    let start = format!("MAX(CAST({} AS INTEGER), 1)", a(2));
                    C::new(
                        format!(
                            "(CASE WHEN instr(substr({1}, {start}), {0}) = 0 THEN 0 ELSE instr(substr({1}, {start}), {0}) + {start} - 1 END)",
                            a(0),
                            a(1)
                        ),
                        Num,
                    )
                } else {
                    C::new(format!("instr({}, {})", a(1), a(0)), Num)
                }
            }
            "SUBSTITUTE" => {
                n_args(3..=3)?;
                C::new(format!("replace({}, {}, {})", a(0), a(1), a(2)), Text)
            }
            "REGEX" => {
                n_args(2..=2)?;
                C::new(format!("lf_regex({}, {})", a(0), a(1)), Bool)
            }
            "HYPERLINK" => {
                n_args(2..=3)?;
                C::new(format!("('<a href=\"' || {} || '\" target=\"_blank\">' || {} || '</a>')", a(0), a(1)), Text)
            }
            "ROUND" => {
                n_args(2..=2)?;
                C::new(
                    format!(
                        "(CASE WHEN CAST({1} AS INTEGER) >= 0 THEN round({0}, CAST({1} AS INTEGER)) ELSE lf_round({0}, CAST({1} AS INTEGER)) END)",
                        a(0),
                        a(1)
                    ),
                    Num,
                )
            }
            // FLOOR and CEILING round towards and away from zero; MFLOOR and MCEILING down and up.
            "FLOOR" | "CEILING" | "MFLOOR" | "MCEILING" => {
                n_args(1..=1)?;
                let f = match name {
                    "FLOOR" => "lf_trunc",
                    "CEILING" => "lf_ceil_away",
                    "MFLOOR" => "lf_floor",
                    _ => "lf_ceil",
                };
                C::new(format!("{f}({})", a(0)), Num)
            }
            "GEOLOCATION" => {
                n_args(2..=2)?;
                expect(&compiled[0], &[Num], name)?;
                expect(&compiled[1], &[Num], name)?;
                C::new(format!("{}, {}", a(0), a(1)), Geo)
            }
            "DISTANCE" => {
                n_args(3..=3)?;
                if compiled[0].kind != Geo || compiled[1].kind != Geo {
                    return Err("DISTANCE() needs two GEOLOCATION() values".into());
                }
                let unit = match &args[2] {
                    Ast::Str(u) if u == "mi" || u == "km" => quote(u),
                    _ => return Err("DISTANCE()'s unit must be 'mi' or 'km'".into()),
                };
                C::new(format!("lf_distance({}, {}, {unit})", a(0), a(1)), Num)
            }
            "ABS" => {
                n_args(1..=1)?;
                C::new(format!("abs({})", a(0)), Num)
            }
            "SQRT" | "EXP" | "LN" | "LOG" => {
                n_args(1..=1)?;
                let f = match name {
                    "SQRT" => "lf_sqrt",
                    "EXP" => "lf_exp",
                    "LN" => "lf_ln",
                    _ => "lf_log10",
                };
                C::new(format!("{f}({})", a(0)), Num)
            }
            "MOD" => {
                n_args(2..=2)?;
                C::new(format!("({0} - {1} * CAST({0} * 1.0 / NULLIF({1}, 0) AS INTEGER))", a(0), a(1)), Num)
            }
            "MAX" | "MIN" => {
                n_args(1..=usize::MAX)?;
                let parts: Vec<String> = compiled.iter().map(|c| c.sql.clone()).collect();
                if parts.len() == 1 {
                    C::new(parts[0].clone(), Num)
                } else {
                    C::new(format!("{}({})", name.to_ascii_lowercase(), parts.join(", ")), first_kind(&compiled))
                }
            }
            "TODAY" => {
                n_args(0..=0)?;
                C::new(quote(&self.env.now.format("%Y-%m-%d").to_string()), Date)
            }
            "NOW" => {
                n_args(0..=0)?;
                C::new(quote(&format_datetime(self.env.now)), DateTime)
            }
            "DATE" => {
                n_args(3..=3)?;
                C::new(
                    format!(
                        "date(printf('%04d-%02d-%02d', CAST({} AS INTEGER), CAST({} AS INTEGER), CAST({} AS INTEGER)))",
                        a(0),
                        a(1),
                        a(2)
                    ),
                    Date,
                )
            }
            "DATEVALUE" => {
                n_args(1..=1)?;
                C::new(format!("date(substr({}, 1, 10))", a(0)), Date)
            }
            "DATETIMEVALUE" => {
                n_args(1..=1)?;
                let s = match compiled[0].kind {
                    Date => format!("({} || 'T00:00:00.000+0000')", a(0)),
                    DateTime => a(0),
                    _ => datetime_from_jd(&format!("julianday(replace(replace({}, 'Z', ''), 'T', ' '))", a(0))),
                };
                C::new(s, DateTime)
            }
            "YEAR" | "MONTH" | "DAY" => {
                n_args(1..=1)?;
                let fmt = match name {
                    "YEAR" => "%Y",
                    "MONTH" => "%m",
                    _ => "%d",
                };
                C::new(format!("CAST(strftime('{fmt}', substr({}, 1, 10)) AS INTEGER)", a(0)), Num)
            }
            "DAYOFYEAR" => {
                n_args(1..=1)?;
                C::new(format!("CAST(strftime('%j', substr({}, 1, 10)) AS INTEGER)", a(0)), Num)
            }
            "ISOWEEK" | "ISOYEAR" => {
                n_args(1..=1)?;
                C::new(format!("lf_{}({})", name.to_ascii_lowercase(), a(0)), Num)
            }
            "UNIXTIMESTAMP" => {
                n_args(1..=1)?;
                let s = match compiled[0].kind {
                    Time => format!("(lf_time_ms({}) / 1000)", a(0)),
                    _ => format!("CAST(strftime('%s', substr({}, 1, 19)) AS INTEGER)", a(0)),
                };
                C::new(s, Num)
            }
            "FROMUNIXTIME" => {
                n_args(1..=1)?;
                C::new(
                    format!("(strftime('%Y-%m-%dT%H:%M:%f', CAST({} AS INTEGER), 'unixepoch') || '+0000')", a(0)),
                    DateTime,
                )
            }
            "TIMEVALUE" => {
                n_args(1..=1)?;
                let s = match compiled[0].kind {
                    DateTime => format!("(substr({}, 12, 12) || 'Z')", a(0)),
                    Time => a(0),
                    _ => format!("NULLIF(lf_timevalue({}), '')", a(0)),
                };
                C::new(s, Time)
            }
            "TIMENOW" => {
                n_args(0..=0)?;
                C::new(quote(&self.env.now.format("%H:%M:%S%.3fZ").to_string()), Time)
            }
            "HOUR" | "MINUTE" | "SECOND" | "MILLISECOND" => {
                n_args(1..=1)?;
                let (at, len) = match name {
                    "HOUR" => (1, 2),
                    "MINUTE" => (4, 2),
                    "SECOND" => (7, 2),
                    _ => (10, 3),
                };
                C::new(format!("CAST(substr({}, {at}, {len}) AS INTEGER)", a(0)), Num)
            }
            "WEEKDAY" => {
                n_args(1..=1)?;
                C::new(format!("(CAST(strftime('%w', substr({}, 1, 10)) AS INTEGER) + 1)", a(0)), Num)
            }
            "ADDMONTHS" => {
                n_args(2..=2)?;
                C::new(format!("lf_addmonths({}, CAST({} AS INTEGER))", a(0), a(1)), compiled[0].kind)
            }
            _ => return Err(format!("Unknown function {name}. Check spelling.")),
        })
    }
}

fn expect(c: &C, kinds: &[Kind], op: &str) -> Result<(), String> {
    if c.kind == Kind::Null || kinds.contains(&c.kind) {
        Ok(())
    } else {
        Err(format!("Incorrect parameter type for operator '{op}'"))
    }
}

/// A value as text, for `&`.
fn text_of(c: &C) -> String {
    match c.kind {
        Kind::Num => num_text(&c.sql),
        _ => format!("IFNULL({}, '')", c.sql),
    }
}

/// A compound address as JSON text, or null when every part is blank.
fn address_sql(prefix: &str, alias: &str) -> String {
    const PARTS: &[(&str, &str)] = &[
        ("city", "City"),
        ("country", "Country"),
        ("geocodeAccuracy", "GeocodeAccuracy"),
        ("latitude", "Latitude"),
        ("longitude", "Longitude"),
        ("postalCode", "PostalCode"),
        ("state", "State"),
        ("street", "Street"),
    ];
    let col = |c: &str| format!("{alias}.{}", qi(&format!("{prefix}{c}")));
    let pairs: Vec<String> = PARTS.iter().map(|(k, c)| format!("'{k}', {}", col(c))).collect();
    let any: Vec<String> = PARTS.iter().map(|(_, c)| format!("{} IS NOT NULL", col(c))).collect();
    format!("CASE WHEN {} THEN json_object({}) END", any.join(" OR "), pairs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_precedence() {
        let ast = parse("1 + 2 * 3 > 6 && !ISBLANK(Name)").unwrap();
        assert!(matches!(ast, Ast::Bin("&&", _, _)));
        assert!(parse("IF(a, b").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn functions_run_in_sqlite() {
        let conn = Connection::open_in_memory().unwrap();
        register_functions(&conn).unwrap();
        let one = |sql: &str| conn.query_row(&format!("SELECT {sql}"), [], |r| r.get::<_, Sql>(0)).unwrap();
        assert_eq!(one("lf_addmonths('2024-01-31', 1)"), Sql::Text("2024-02-29".into()));
        assert_eq!(one("lf_addmonths('2024-01-15', -1)"), Sql::Text("2023-12-15".into()));
        assert_eq!(one("lf_regex('AB-12', '[A-Z]{2}-\\d+')"), Sql::Integer(1));
        assert_eq!(one("lf_regex('xAB-12', '[A-Z]{2}-\\d+')"), Sql::Integer(0));
        assert_eq!(one("lf_pow(2, 10)"), Sql::Real(1024.0));
        assert_eq!(one(&num_text("12.0")), Sql::Text("12".into()));
        for (text, is_number) in
            [("123", 1), ("-3.5", 1), ("2E2", 1), ("123.", 1), (" 12 ", 0), ("1 000", 0), ("123,12", 0), ("", 0)]
        {
            assert_eq!(one(&format!("lf_isnumber('{text}')")), Sql::Integer(is_number), "{text}");
        }
        assert_eq!(js_encode("<B>user's</b>"), "\\u003CB\\u003Euser\\'s\\u003C\\/b\\u003E");
        assert_eq!(html_encode("<B>user's</b>"), "&lt;B&gt;user&#39;s&lt;/b&gt;");
    }
}
