//! SOSL: `FIND {term} IN … FIELDS RETURNING Account(Id, Name WHERE …), …`.
//!
//! Search matches whole words, as Salesforce's search index does: text is
//! split into words at anything that isn't a letter or digit, a search word
//! matches a word (case-insensitively), `*` and `?` are wildcards within a
//! word, and a search word with punctuation in it (`555-0199`,
//! `jo.smith@acme.com`) is a phrase of its words. Every word of the term
//! must match somewhere in the record's searched fields.
//!
//! The matching records' Ids then go to a SOQL query built from the
//! object's RETURNING clause, so its WHERE, ORDER BY and LIMIT work exactly
//! as in SOQL. Without an ORDER BY, records come back by relevance: more
//! matches first, matches in the name field counting most, then the most
//! recently modified. Objects come back in RETURNING order.

use std::collections::HashMap;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{SfError, SfResult};
use crate::schema::{Catalog, FieldType, SObject};
use crate::soql::exec::run_parsed;
use crate::soql::parse::{Cond, Expr, InSource, Literal};
use crate::store::Ctx;
use crate::store::qi;

fn malformed(msg: impl Into<String>) -> SfError {
    SfError::new("MALFORMED_SEARCH", msg)
}

/// Which fields a search looks in.
#[derive(Clone, Copy, PartialEq)]
enum Scope {
    All,
    Name,
    Email,
    Phone,
}

struct Returning {
    object: String,
    /// What's inside the parentheses: fields, then WHERE / ORDER BY / LIMIT / OFFSET.
    clause: Option<String>,
}

struct Search {
    term: String,
    scope: Scope,
    returning: Vec<Returning>,
    limit: usize,
}

/// Run a SOSL search, returning `{"searchRecords": [...]}`.
pub fn search(conn: &Connection, cat: &Catalog, ctx: &Ctx, sosl: &str) -> SfResult<Value> {
    let s = parse(sosl)?;
    let term = parse_term(&s.term)?;
    let targets: Vec<Returning> = if s.returning.is_empty() {
        cat.objects
            .iter()
            .filter(|o| o.name != "User" && !search_fields(o, s.scope).is_empty())
            .map(|o| Returning { object: o.name.clone(), clause: None })
            .collect()
    } else {
        s.returning
    };
    let mut records = vec![];
    for r in targets {
        let obj = cat
            .object(&r.object)
            .ok_or_else(|| SfError::new("INVALID_TYPE", format!("sObject type '{}' is not supported.", r.object)))?;
        let fields = search_fields(obj, s.scope);
        if fields.is_empty() {
            continue;
        }
        let ranked = rank(conn, obj, &fields, &term)?;
        let (select, rest) = match &r.clause {
            Some(c) => split_clause(c),
            None => ("Id".to_string(), String::new()),
        };
        let soql = format!("SELECT {select} FROM {} {rest}", obj.name);
        let mut q = crate::soql::parse::parse(&soql)?;
        // An Id that can't exist keeps the IN list valid when nothing matched.
        let ids = if ranked.is_empty() { vec!["000000000000000AAA".to_string()] } else { ranked.clone() };
        let cond = Cond::In {
            lhs: Expr::Path { parts: vec!["Id".into()], pos: 0 },
            negated: false,
            source: InSource::List(ids.into_iter().map(Literal::Str).collect()),
        };
        q.where_ = Some(match q.where_.take() {
            Some(w) => Cond::And(vec![cond, w]),
            None => cond,
        });
        let by_relevance = q.order_by.is_empty();
        let (limit, offset) = (q.limit, q.offset);
        if by_relevance {
            q.limit = None;
            q.offset = None;
        }
        let mut found = run_parsed(conn, cat, ctx, &soql, &q, false, 2000)?.records;
        if by_relevance {
            let place: HashMap<&str, usize> = ranked.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            found.sort_by_key(|rec| record_id(rec).and_then(|id| place.get(id).copied()).unwrap_or(usize::MAX));
            let skip = offset.unwrap_or(0) as usize;
            found = found.into_iter().skip(skip).take(limit.map_or(usize::MAX, |l| l as usize)).collect();
        }
        records.extend(found);
        if records.len() >= s.limit {
            records.truncate(s.limit);
            break;
        }
    }
    Ok(json!({ "searchRecords": records }))
}

/// A search result's Id, from its `attributes.url` (the Id itself may not be selected).
fn record_id(rec: &Value) -> Option<&str> {
    rec["attributes"]["url"].as_str().and_then(|u| u.rsplit('/').next())
}

/// The other forms Salesforce's index keeps of a phone number or e-mail address,
/// so `5551234567` finds `(555) 123-4567` and `johnsmith` finds `john.smith@x.com`.
fn extra_tokens(text: &str, ty: Option<FieldType>) -> Vec<String> {
    match ty {
        Some(FieldType::Phone) => {
            let digits: String = text.chars().filter(char::is_ascii_digit).collect();
            let mut out = vec![digits.clone()];
            for keep in [10, 7] {
                if digits.len() > keep {
                    out.push(digits[digits.len() - keep..].to_string());
                }
            }
            out
        }
        Some(FieldType::Email) => {
            let local = text.split('@').next().unwrap_or("");
            vec![local.chars().filter(|c| c.is_alphanumeric()).collect()]
        }
        _ => vec![],
    }
}

/// The Ids of `obj`'s records that match the term, most relevant first.
fn rank(conn: &Connection, obj: &SObject, fields: &[String], term: &Term) -> SfResult<Vec<String>> {
    let names: Vec<String> = search_fields(obj, Scope::Name);
    let cols: Vec<String> = fields.iter().map(|f| qi(f)).collect();
    let recent = if obj.field("LastModifiedDate").is_some() { " ORDER BY \"LastModifiedDate\" DESC" } else { "" };
    let live = if obj.field("IsDeleted").is_some() { " WHERE \"IsDeleted\" = 0" } else { "" };
    let sql = format!("SELECT \"Id\", {} FROM {}{live}{recent}", cols.join(", "), qi(&obj.name));
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut scored = vec![];
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let mut texts = vec![];
        for (i, f) in fields.iter().enumerate() {
            if let Some(text) = row.get::<_, Option<String>>(i + 1)? {
                let weight = if names.contains(f) { 3 } else { 1 };
                let ty = obj.field(f).map(|f| f.ty);
                texts.extend(extra_tokens(&text, ty).into_iter().map(|t| (t, weight)));
                texts.push((text, weight));
            }
        }
        if let Some(score) = term.score(&texts) {
            scored.push((score, id));
        }
    }
    // A stable sort keeps the most recently modified first among equals.
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    Ok(scored.into_iter().take(2000).map(|(_, id)| id).collect())
}

/// `parameterizedSearch` GET parameters, as SOSL.
pub fn from_params(p: &HashMap<String, String>) -> SfResult<String> {
    let q = p.get("q").filter(|q| !q.trim().is_empty()).ok_or_else(|| malformed("q: The q parameter is required"))?;
    let objects: Vec<String> = p
        .get("sobject")
        .map(|s| s.split(',').map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect())
        .unwrap_or_default();
    let specs = objects
        .iter()
        .map(|o| {
            let get = |k: &str| p.get(&format!("{o}.{k}")).cloned();
            spec(
                o,
                get("fields").or_else(|| p.get("fields").cloned()),
                get("where"),
                get("limit").or_else(|| p.get("defaultLimit").cloned()),
            )
        })
        .collect::<Vec<_>>();
    Ok(build(q, p.get("in").map(String::as_str), &specs, p.get("overallLimit").map(String::as_str)))
}

/// `parameterizedSearch` POST body, as SOSL.
pub fn from_json(b: &Value) -> SfResult<String> {
    let q =
        b["q"].as_str().filter(|q| !q.trim().is_empty()).ok_or_else(|| malformed("q: The q property is required"))?;
    let list = |v: &Value| v.as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
    let default_fields = list(&b["fields"]);
    let num = |v: &Value| v.as_u64().map(|n| n.to_string());
    let specs = b["sobjects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let name = o["name"].as_str()?;
            Some(spec(
                name,
                list(&o["fields"]).or_else(|| default_fields.clone()),
                o["where"].as_str().map(String::from),
                num(&o["limit"]).or_else(|| num(&b["defaultLimit"])),
            ))
        })
        .collect::<Vec<_>>();
    let overall = num(&b["overallLimit"]);
    Ok(build(q, b["in"].as_str(), &specs, overall.as_deref()))
}

fn spec(object: &str, fields: Option<String>, where_: Option<String>, limit: Option<String>) -> String {
    let mut inner = fields.unwrap_or_else(|| "Id".into());
    if let Some(w) = where_ {
        inner += &format!(" WHERE {w}");
    }
    if let Some(l) = limit {
        inner += &format!(" LIMIT {l}");
    }
    format!("{object}({inner})")
}

fn build(q: &str, scope: Option<&str>, specs: &[String], limit: Option<&str>) -> String {
    let mut escaped = String::new();
    for c in q.chars() {
        if matches!(c, '{' | '}' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    let mut s = format!("FIND {{{escaped}}} IN {} FIELDS", scope.unwrap_or("ALL").to_ascii_uppercase());
    if !specs.is_empty() {
        s += &format!(" RETURNING {}", specs.join(", "));
    }
    if let Some(l) = limit {
        s += &format!(" LIMIT {l}");
    }
    s
}

/// The fields a scope searches on an object.
fn search_fields(o: &SObject, scope: Scope) -> Vec<String> {
    let searchable = |ty: FieldType| {
        matches!(ty, FieldType::String | FieldType::Textarea | FieldType::Email | FieldType::Phone | FieldType::Url)
    };
    let fields = o.fields.iter().filter(|f| !f.is_computed() && searchable(f.ty));
    match scope {
        Scope::All => fields.map(|f| f.name.clone()).collect(),
        Scope::Email => fields.filter(|f| f.ty == FieldType::Email).map(|f| f.name.clone()).collect(),
        Scope::Phone => fields.filter(|f| f.ty == FieldType::Phone).map(|f| f.name.clone()).collect(),
        Scope::Name => {
            let mut names: Vec<String> = ["Name", "FirstName", "LastName", "Subject"]
                .iter()
                .filter_map(|n| o.field(n).filter(|f| searchable(f.ty)).map(|f| f.name.clone()))
                .collect();
            if let Some(f) = o.name_field()
                && !names.contains(&f.name)
            {
                names.push(f.name.clone());
            }
            names
        }
    }
}

/// Split `Id, Name WHERE … LIMIT 5` into the field list and the rest.
fn split_clause(c: &str) -> (String, String) {
    let upper = c.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut in_quote = false;
    let mut depth = 0;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'\'' if i == 0 || bytes[i - 1] != b'\\' => in_quote = !in_quote,
            b'(' if !in_quote => depth += 1,
            b')' if !in_quote => depth -= 1,
            _ if in_quote || depth > 0 => {}
            _ => {
                let at_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
                if at_word
                    && ["WHERE", "ORDER", "LIMIT", "OFFSET", "USING"].iter().any(|kw| {
                        upper[i..].starts_with(kw)
                            && upper[i + kw.len()..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric())
                    })
                {
                    let fields = c[..i].trim();
                    return (if fields.is_empty() { "Id".into() } else { fields.into() }, c[i..].to_string());
                }
            }
        }
    }
    let fields = c.trim();
    (if fields.is_empty() { "Id".into() } else { fields.into() }, String::new())
}

// Parsing the SOSL statement

struct Cursor<'a> {
    src: &'a str,
    i: usize,
}

impl Cursor<'_> {
    fn ws(&mut self) {
        while self.src[self.i..].starts_with(|c: char| c.is_whitespace()) {
            self.i += 1;
        }
    }
    fn kw(&mut self, kw: &str) -> bool {
        self.ws();
        let rest = &self.src[self.i..];
        if rest.len() >= kw.len()
            && rest[..kw.len()].eq_ignore_ascii_case(kw)
            && rest[kw.len()..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
        {
            self.i += kw.len();
            true
        } else {
            false
        }
    }
    fn word(&mut self) -> Option<String> {
        self.ws();
        let rest = &self.src[self.i..];
        let n = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        if n == 0 {
            return None;
        }
        self.i += n;
        Some(rest[..n].to_string())
    }
    fn eat(&mut self, c: char) -> bool {
        self.ws();
        if self.src[self.i..].starts_with(c) {
            self.i += c.len_utf8();
            true
        } else {
            false
        }
    }
    fn done(&mut self) -> bool {
        self.ws();
        self.i >= self.src.len()
    }
    fn rest(&self) -> &str {
        &self.src[self.i..]
    }
}

fn parse(src: &str) -> SfResult<Search> {
    let mut c = Cursor { src, i: 0 };
    if !c.kw("FIND") {
        return Err(malformed("No search term found. The search term must be enclosed in braces."));
    }
    c.ws();
    let term = match c.rest().chars().next() {
        Some(open @ ('{' | '\'')) => {
            let close = if open == '{' { '}' } else { '\'' };
            let body = &c.rest()[1..];
            let mut term = String::new();
            let mut chars = body.char_indices();
            let mut end = None;
            while let Some((i, ch)) = chars.next() {
                if ch == '\\' {
                    if let Some((_, next)) = chars.next() {
                        term.push('\\');
                        term.push(next);
                    }
                } else if ch == close {
                    end = Some(i);
                    break;
                } else {
                    term.push(ch);
                }
            }
            let end = end.ok_or_else(|| malformed(format!("The search term isn't closed with '{close}'")))?;
            c.i += 1 + end + 1;
            term
        }
        _ => return Err(malformed("No search term found. The search term must be enclosed in braces.")),
    };
    let mut s = Search { term, scope: Scope::All, returning: vec![], limit: 2000 };
    while !c.done() {
        if c.kw("IN") {
            s.scope = match c.word().map(|w| w.to_ascii_uppercase()).as_deref() {
                Some("ALL" | "SIDEBAR") => Scope::All,
                Some("NAME") => Scope::Name,
                Some("EMAIL") => Scope::Email,
                Some("PHONE") => Scope::Phone,
                _ => return Err(malformed("IN must be followed by ALL, NAME, EMAIL, PHONE or SIDEBAR FIELDS")),
            };
            if !c.kw("FIELDS") {
                return Err(malformed("expected FIELDS"));
            }
        } else if c.kw("RETURNING") {
            loop {
                let object = c.word().ok_or_else(|| malformed("RETURNING needs an object"))?;
                let clause = if c.eat('(') {
                    let start = c.i;
                    let mut depth = 1;
                    let mut in_quote = false;
                    let bytes = src.as_bytes();
                    while c.i < src.len() {
                        match bytes[c.i] {
                            b'\\' if in_quote => c.i += 1,
                            b'\'' => in_quote = !in_quote,
                            b'(' if !in_quote => depth += 1,
                            b')' if !in_quote => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        c.i += 1;
                    }
                    if depth != 0 {
                        return Err(malformed(format!("RETURNING {object}(…) isn't closed")));
                    }
                    let inner = src[start..c.i].to_string();
                    c.i += 1;
                    Some(inner)
                } else {
                    None
                };
                s.returning.push(Returning { object, clause });
                if !c.eat(',') {
                    break;
                }
            }
        } else if c.kw("WITH") {
            // WITH SNIPPET, HIGHLIGHT, METADATA, SPELL_CORRECTION, NETWORK, DIVISION, PricebookId, DATA CATEGORY:
            // nothing to do, so skip to the next clause.
            while !c.done() && !["RETURNING", "LIMIT", "WITH", "UPDATE", "IN"].iter().any(|k| starts_kw(c.rest(), k)) {
                let ch = c.rest().chars().next().unwrap();
                c.i += ch.len_utf8();
                if ch == '\'' {
                    while !c.done() && !c.rest().starts_with('\'') {
                        c.i += c.rest().chars().next().unwrap().len_utf8();
                    }
                    c.i += 1;
                }
            }
        } else if c.kw("LIMIT") {
            s.limit = c.word().and_then(|n| n.parse().ok()).ok_or_else(|| malformed("LIMIT needs a number"))?;
        } else if c.kw("UPDATE") {
            c.word();
        } else {
            return Err(malformed(format!("unexpected token: '{}'", c.rest().split_whitespace().next().unwrap_or(""))));
        }
    }
    Ok(s)
}

fn starts_kw(s: &str, kw: &str) -> bool {
    s.len() >= kw.len()
        && s[..kw.len()].eq_ignore_ascii_case(kw)
        && s[kw.len()..].chars().next().is_none_or(|c| !c.is_ascii_alphanumeric())
}

// The search term: words, "phrases", AND, OR, AND NOT, parentheses, * and ? wildcards.

enum Term {
    Word(regex::Regex),
    And(Box<Term>, Box<Term>),
    AndNot(Box<Term>, Box<Term>),
    Or(Box<Term>, Box<Term>),
}

impl Term {
    /// How well a record's searched texts (with each field's weight) match, or None if they don't.
    fn score(&self, texts: &[(String, u32)]) -> Option<u32> {
        match self {
            Term::Word(re) => {
                let n: u32 = texts.iter().map(|(t, w)| re.find_iter(t).count() as u32 * w).sum();
                (n > 0).then_some(n)
            }
            Term::And(a, b) => Some(a.score(texts)? + b.score(texts)?),
            Term::AndNot(a, b) => match b.score(texts) {
                Some(_) => None,
                None => a.score(texts),
            },
            Term::Or(a, b) => match (a.score(texts), b.score(texts)) {
                (None, None) => None,
                (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
            },
        }
    }
}

/// A search word (or phrase) as a regex over text: its words in order, each
/// a whole word, with `*` and `?` standing for any letters or digits.
fn word_regex(w: &str) -> SfResult<regex::Regex> {
    const ALNUM: &str = r"[\p{L}\p{N}]";
    const GAP: &str = r"[^\p{L}\p{N}]+";
    let mut words: Vec<String> = vec![];
    let mut cur = String::new();
    let mut chars = w.chars();
    let flush = |cur: &mut String, words: &mut Vec<String>| {
        if !cur.is_empty() {
            words.push(std::mem::take(cur));
        }
    };
    while let Some(ch) = chars.next() {
        match ch {
            '*' => cur.push_str(&format!("{ALNUM}*")),
            '?' => cur.push_str(ALNUM),
            '\\' => {
                if let Some(next) = chars.next() {
                    if next.is_alphanumeric() {
                        cur.push_str(&regex::escape(&next.to_string()));
                    } else {
                        flush(&mut cur, &mut words);
                    }
                }
            }
            c if c.is_alphanumeric() => cur.push_str(&regex::escape(&c.to_string())),
            _ => flush(&mut cur, &mut words),
        }
    }
    flush(&mut cur, &mut words);
    if words.is_empty() {
        return Err(SfError::new("INVALID_SEARCH", format!("search term '{w}' has no words to search for")));
    }
    let body = words.join(GAP);
    regex::RegexBuilder::new(&format!(r"(?:^|[^\p{{L}}\p{{N}}]){body}(?:$|[^\p{{L}}\p{{N}}])"))
        .case_insensitive(true)
        .build()
        .map_err(|e| malformed(format!("invalid search term: {e}")))
}

#[derive(Debug, PartialEq)]
enum TTok {
    Word(String),
    Phrase(String),
    LParen,
    RParen,
}

fn parse_term(src: &str) -> SfResult<Term> {
    let mut toks = vec![];
    let mut chars = src.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                toks.push(TTok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(TTok::RParen);
            }
            '"' => {
                chars.next();
                let mut p = String::new();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    p.push(c);
                }
                toks.push(TTok::Phrase(p));
            }
            _ => {
                let mut w = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '(' || c == ')' || c == '"' {
                        break;
                    }
                    chars.next();
                    w.push(c);
                    if c == '\\'
                        && let Some(n) = chars.next()
                    {
                        w.push(n);
                    }
                }
                toks.push(TTok::Word(w));
            }
        }
    }
    let longest = toks
        .iter()
        .map(|t| match t {
            TTok::Word(w) if !["AND", "OR", "NOT"].contains(&w.to_ascii_uppercase().as_str()) => {
                w.chars().filter(|c| !matches!(c, '*' | '?' | '\\')).count()
            }
            TTok::Phrase(p) => p.chars().count(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    if longest < 2 {
        return Err(SfError::new("INVALID_SEARCH", "search term must be longer than one character"));
    }
    let mut p = TermParser { toks, i: 0 };
    let t = p.or()?;
    if p.i < p.toks.len() {
        return Err(malformed("unbalanced parentheses in the search term"));
    }
    Ok(t)
}

struct TermParser {
    toks: Vec<TTok>,
    i: usize,
}

impl TermParser {
    fn is_op(&self, op: &str) -> bool {
        matches!(self.toks.get(self.i), Some(TTok::Word(w)) if w.eq_ignore_ascii_case(op))
    }
    fn or(&mut self) -> SfResult<Term> {
        let mut t = self.and()?;
        while self.is_op("OR") {
            self.i += 1;
            t = Term::Or(Box::new(t), Box::new(self.and()?));
        }
        Ok(t)
    }
    fn and(&mut self) -> SfResult<Term> {
        let mut t = self.atom()?;
        loop {
            if self.is_op("AND") {
                self.i += 1;
                if self.is_op("NOT") {
                    self.i += 1;
                    t = Term::AndNot(Box::new(t), Box::new(self.atom()?));
                } else {
                    t = Term::And(Box::new(t), Box::new(self.atom()?));
                }
            } else if self.is_op("NOT") {
                self.i += 1;
                t = Term::AndNot(Box::new(t), Box::new(self.atom()?));
            } else if matches!(self.toks.get(self.i), Some(TTok::Word(_) | TTok::Phrase(_) | TTok::LParen))
                && !self.is_op("OR")
            {
                // Words next to each other must all match.
                t = Term::And(Box::new(t), Box::new(self.atom()?));
            } else {
                return Ok(t);
            }
        }
    }
    fn atom(&mut self) -> SfResult<Term> {
        let tok = self.toks.get(self.i).ok_or_else(|| malformed("the search term ends too soon"))?;
        self.i += 1;
        match tok {
            TTok::Word(w) | TTok::Phrase(w) => Ok(Term::Word(word_regex(w)?)),
            TTok::LParen => {
                let t = self.or()?;
                if self.toks.get(self.i) != Some(&TTok::RParen) {
                    return Err(malformed("unbalanced parentheses in the search term"));
                }
                self.i += 1;
                Ok(t)
            }
            TTok::RParen => Err(malformed("unbalanced parentheses in the search term")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_statements() {
        let s = parse(
            "FIND {Acme*} IN NAME FIELDS RETURNING Account(Id, Name WHERE Industry = 'x' LIMIT 5), Contact LIMIT 10",
        )
        .unwrap();
        assert_eq!(s.term, "Acme*");
        assert!(s.scope == Scope::Name);
        assert_eq!(s.returning.len(), 2);
        assert_eq!(s.returning[0].clause.as_deref(), Some("Id, Name WHERE Industry = 'x' LIMIT 5"));
        assert_eq!(s.limit, 10);
        assert_eq!(split_clause("Id, Name WHERE Industry = 'x' LIMIT 5").0, "Id, Name");
        assert!(parse("FIND Acme").is_err());
        assert!(parse_term("a").is_err());
        assert_eq!(parse("FIND {a\\}b}").unwrap().term, "a\\}b");
    }
}
