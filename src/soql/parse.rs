//! A hand-written lexer and recursive-descent parser for SOQL.
//!
//! SOQL is small enough that a parser generator isn't worth it, and owning
//! the parser lets errors carry Salesforce's `ERROR at Row:1:Column:N` shape.

use crate::error::SfError;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Date(String),
    DateTime(String),
    LParen,
    RParen,
    Comma,
    Dot,
    Colon,
    Op(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub pos: usize,
}

#[derive(Clone, Debug)]
pub struct Query {
    pub select: Vec<SelectItem>,
    pub from: String,
    pub from_pos: usize,
    pub alias: Option<String>,
    pub where_: Option<Cond>,
    pub group_by: Vec<Expr>,
    pub group_mode: GroupMode,
    pub having: Option<Cond>,
    pub order_by: Vec<OrderItem>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    /// `FOR VIEW`, `FOR REFERENCE` or `FOR UPDATE`.
    pub for_: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroupMode {
    Plain,
    Rollup,
    Cube,
}

#[derive(Clone, Debug)]
pub enum SelectItem {
    Expr {
        expr: Expr,
        alias: Option<String>,
    },
    Sub(Box<Query>),
    Fields {
        kind: String,
        pos: usize,
    },
    /// `TYPEOF What WHEN Account THEN Name, Phone ELSE Name END`
    TypeOf {
        rel: String,
        pos: usize,
        whens: Vec<(String, usize, Vec<Expr>)>,
        else_: Vec<Expr>,
    },
}

#[derive(Clone, Debug)]
pub enum Expr {
    Path { parts: Vec<String>, pos: usize },
    Func { name: String, args: Vec<Expr>, pos: usize },
}

impl Expr {
    pub fn pos(&self) -> usize {
        match self {
            Expr::Path { pos, .. } | Expr::Func { pos, .. } => *pos,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OrderItem {
    pub expr: Expr,
    pub desc: bool,
    pub nulls_last: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum Literal {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Date(String),
    DateTime(String),
    /// `TODAY`, `LAST_N_DAYS:30`, …
    DateLiteral(String, Option<i64>),
}

#[derive(Clone, Debug)]
pub enum InSource {
    List(Vec<Literal>),
    Query(Box<Query>),
}

#[derive(Clone, Debug)]
pub enum Cond {
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(Box<Cond>),
    Cmp { lhs: Expr, op: &'static str, rhs: Literal, pos: usize },
    In { lhs: Expr, negated: bool, source: InSource },
    Includes { lhs: Expr, excludes: bool, values: Vec<String>, pos: usize },
}

const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "LIKE", "INCLUDES", "EXCLUDES", "GROUP", "BY", "HAVING",
    "ORDER", "ASC", "DESC", "NULLS", "FIRST", "LAST", "LIMIT", "OFFSET", "WITH", "USING", "FOR", "TRUE", "FALSE",
    "NULL", "TYPEOF", "WHEN", "THEN", "ELSE", "END",
];

/// Salesforce's error format: the query, a caret, and the row and column.
pub fn query_error(code: &'static str, soql: &str, pos: usize, msg: &str) -> SfError {
    let before = &soql[..pos.min(soql.len())];
    let row = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = before[line_start..].chars().count() + 1;
    let line = soql[line_start..].lines().next().unwrap_or("");
    SfError::new(code, format!("\n{line}\n{}^\nERROR at Row:{row}:Column:{col}\n{msg}", " ".repeat(col - 1)))
}

pub fn lex(src: &str) -> Result<Vec<Token>, SfError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    let err = |pos: usize, m: &str| query_error("MALFORMED_QUERY", src, pos, m);
    while i < b.len() {
        let c = b[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let tok = match c {
            b'(' => {
                i += 1;
                Tok::LParen
            }
            b')' => {
                i += 1;
                Tok::RParen
            }
            b',' => {
                i += 1;
                Tok::Comma
            }
            b'.' => {
                i += 1;
                Tok::Dot
            }
            b':' => {
                i += 1;
                Tok::Colon
            }
            b'=' => {
                i += 1;
                Tok::Op("=")
            }
            b'!' if b.get(i + 1) == Some(&b'=') => {
                i += 2;
                Tok::Op("!=")
            }
            b'<' => match b.get(i + 1) {
                Some(b'=') => {
                    i += 2;
                    Tok::Op("<=")
                }
                Some(b'>') => {
                    i += 2;
                    Tok::Op("!=")
                }
                _ => {
                    i += 1;
                    Tok::Op("<")
                }
            },
            b'>' => {
                if b.get(i + 1) == Some(&b'=') {
                    i += 2;
                    Tok::Op(">=")
                } else {
                    i += 1;
                    Tok::Op(">")
                }
            }
            b'\'' => {
                i += 1;
                let mut s = String::new();
                loop {
                    let Some(&c) = b.get(i) else { return Err(err(start, "unterminated string literal")) };
                    match c {
                        b'\'' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            let e = *b.get(i + 1).ok_or_else(|| err(i, "invalid escape"))?;
                            match e {
                                b'n' => s.push('\n'),
                                b't' => s.push('\t'),
                                b'r' => s.push('\r'),
                                b'b' => s.push('\u{8}'),
                                b'f' => s.push('\u{c}'),
                                b'\'' | b'"' | b'\\' => s.push(e as char),
                                // Kept escaped: only meaningful to LIKE.
                                b'%' | b'_' => {
                                    s.push('\\');
                                    s.push(e as char);
                                }
                                _ => return Err(err(i, "Invalid string literal: illegal escape sequence")),
                            }
                            i += 2;
                        }
                        _ => {
                            let ch = src[i..].chars().next().unwrap();
                            s.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                Tok::Str(s)
            }
            c if c.is_ascii_digit() || (c == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) => {
                if let Some(len) = date_len(&b[i..]) {
                    let text = &src[i..i + len];
                    i += len;
                    if text.len() > 10 { Tok::DateTime(text.to_string()) } else { Tok::Date(text.to_string()) }
                } else {
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                        i += 1;
                    }
                    Tok::Num(src[start..i].to_string())
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                Tok::Ident(src[start..i].to_string())
            }
            _ => return Err(err(i, &format!("unexpected token: '{}'", src[i..].chars().next().unwrap()))),
        };
        out.push(Token { tok, pos: start });
    }
    out.push(Token { tok: Tok::Eof, pos: src.len() });
    Ok(out)
}

/// Length of a date (`2024-01-31`) or datetime (`2024-01-31T10:00:00.000+0100`) literal at the start of `b`.
fn date_len(b: &[u8]) -> Option<usize> {
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if !(digits(0..4) && b.get(4) == Some(&b'-') && digits(5..7) && b.get(7) == Some(&b'-') && digits(8..10)) {
        return None;
    }
    if b.get(10) != Some(&b'T') {
        return Some(10);
    }
    let mut i = 11;
    if !(digits(11..13) && b.get(13) == Some(&b':') && digits(14..16) && b.get(16) == Some(&b':') && digits(17..19)) {
        return None;
    }
    i += 8;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    match b.get(i) {
        Some(b'Z') => Some(i + 1),
        Some(b'+' | b'-') => {
            i += 1;
            let mut n = 0;
            while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b':') {
                i += 1;
                n += 1;
            }
            (n >= 4).then_some(i)
        }
        _ => None,
    }
}

pub struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    i: usize,
}

pub fn parse(src: &str) -> Result<Query, SfError> {
    let mut p = Parser { src, toks: lex(src)?, i: 0 };
    let q = p.query()?;
    if p.peek() != &Tok::Eof {
        return Err(p.unexpected());
    }
    Ok(q)
}

impl<'a> Parser<'a> {
    fn cur(&self) -> &Token {
        &self.toks[self.i.min(self.toks.len() - 1)]
    }
    fn peek(&self) -> &Tok {
        &self.cur().tok
    }
    fn pos(&self) -> usize {
        self.cur().pos
    }
    fn next(&mut self) -> Token {
        let t = self.cur().clone();
        self.i += 1;
        t
    }
    fn unexpected(&self) -> SfError {
        let t = self.cur();
        let shown = match &t.tok {
            Tok::Eof => "<EOF>".to_string(),
            Tok::Ident(s) | Tok::Num(s) | Tok::Date(s) | Tok::DateTime(s) => s.clone(),
            Tok::Str(s) => format!("'{s}'"),
            Tok::Op(o) => o.to_string(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::Comma => ",".into(),
            Tok::Dot => ".".into(),
            Tok::Colon => ":".into(),
        };
        query_error("MALFORMED_QUERY", self.src, t.pos, &format!("unexpected token: '{shown}'"))
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect_kw(&mut self, kw: &str) -> Result<(), SfError> {
        if self.eat_kw(kw) { Ok(()) } else { Err(self.unexpected()) }
    }
    fn expect(&mut self, t: Tok) -> Result<(), SfError> {
        if *self.peek() == t {
            self.next();
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }
    fn ident(&mut self) -> Result<(String, usize), SfError> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                let pos = self.pos();
                self.next();
                Ok((s, pos))
            }
            _ => Err(self.unexpected()),
        }
    }
    /// An identifier that isn't a reserved word, used for aliases.
    fn alias(&mut self) -> Option<String> {
        match self.peek() {
            Tok::Ident(s) if !KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(s)) => {
                let s = s.clone();
                self.next();
                Some(s)
            }
            _ => None,
        }
    }
    fn uint(&mut self) -> Result<u64, SfError> {
        match self.peek().clone() {
            Tok::Num(n) => {
                self.next();
                n.parse().map_err(|_| self.unexpected())
            }
            _ => Err(self.unexpected()),
        }
    }

    fn query(&mut self) -> Result<Query, SfError> {
        self.expect_kw("SELECT")?;
        let mut select = vec![];
        loop {
            select.push(self.select_item()?);
            if *self.peek() != Tok::Comma {
                break;
            }
            self.next();
        }
        self.expect_kw("FROM")?;
        let (from, from_pos) = self.ident()?;
        let alias = self.alias();
        let mut q = Query {
            select,
            from,
            from_pos,
            alias,
            where_: None,
            group_by: vec![],
            group_mode: GroupMode::Plain,
            having: None,
            order_by: vec![],
            limit: None,
            offset: None,
            for_: None,
        };
        if self.eat_kw("USING") {
            self.expect_kw("SCOPE")?;
            self.ident()?;
        }
        if self.eat_kw("WHERE") {
            q.where_ = Some(self.cond()?);
        }
        if self.eat_kw("WITH") {
            // WITH SECURITY_ENFORCED / USER_MODE / SYSTEM_MODE: no sharing model to enforce.
            self.ident()?;
        }
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            q.group_mode = if self.eat_kw("ROLLUP") {
                GroupMode::Rollup
            } else if self.eat_kw("CUBE") {
                GroupMode::Cube
            } else {
                GroupMode::Plain
            };
            let rollup = q.group_mode != GroupMode::Plain;
            if rollup {
                self.expect(Tok::LParen)?;
            }
            loop {
                q.group_by.push(self.expr()?);
                if *self.peek() != Tok::Comma {
                    break;
                }
                self.next();
            }
            if rollup {
                self.expect(Tok::RParen)?;
            }
            if self.eat_kw("HAVING") {
                q.having = Some(self.cond()?);
            }
        }
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let expr = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                let nulls_last = if self.eat_kw("NULLS") {
                    if self.eat_kw("LAST") {
                        Some(true)
                    } else {
                        self.expect_kw("FIRST")?;
                        Some(false)
                    }
                } else {
                    None
                };
                q.order_by.push(OrderItem { expr, desc, nulls_last });
                if *self.peek() != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        if self.eat_kw("LIMIT") {
            q.limit = Some(self.uint()?);
        }
        if self.eat_kw("OFFSET") {
            q.offset = Some(self.uint()?);
        }
        if self.eat_kw("FOR") {
            // FOR VIEW / FOR REFERENCE / FOR UPDATE
            q.for_ = Some(self.ident()?.0.to_ascii_uppercase());
        }
        Ok(q)
    }

    fn select_item(&mut self) -> Result<SelectItem, SfError> {
        if *self.peek() == Tok::LParen {
            self.next();
            let q = self.query()?;
            self.expect(Tok::RParen)?;
            return Ok(SelectItem::Sub(Box::new(q)));
        }
        if self.is_kw("FIELDS") && matches!(self.toks.get(self.i + 1).map(|t| &t.tok), Some(Tok::LParen)) {
            let pos = self.pos();
            self.next();
            self.next();
            let (kind, _) = self.ident()?;
            self.expect(Tok::RParen)?;
            return Ok(SelectItem::Fields { kind: kind.to_ascii_uppercase(), pos });
        }
        if self.is_kw("TYPEOF") {
            let pos = self.pos();
            self.next();
            let (rel, _) = self.ident()?;
            let mut whens = vec![];
            let mut else_ = vec![];
            while self.eat_kw("WHEN") {
                let (ty, ty_pos) = self.ident()?;
                self.expect_kw("THEN")?;
                whens.push((ty, ty_pos, self.field_list()?));
            }
            if whens.is_empty() {
                return Err(self.unexpected());
            }
            if self.eat_kw("ELSE") {
                else_ = self.field_list()?;
            }
            self.expect_kw("END")?;
            return Ok(SelectItem::TypeOf { rel, pos, whens, else_ });
        }
        let expr = self.expr()?;
        let alias = self.alias();
        Ok(SelectItem::Expr { expr, alias })
    }

    fn field_list(&mut self) -> Result<Vec<Expr>, SfError> {
        let mut out = vec![self.expr()?];
        while *self.peek() == Tok::Comma {
            self.next();
            out.push(self.expr()?);
        }
        Ok(out)
    }

    fn expr(&mut self) -> Result<Expr, SfError> {
        let (first, pos) = self.ident()?;
        if *self.peek() == Tok::LParen {
            self.next();
            let mut args = vec![];
            while *self.peek() != Tok::RParen {
                args.push(self.expr()?);
                if *self.peek() == Tok::Comma {
                    self.next();
                }
            }
            self.expect(Tok::RParen)?;
            return Ok(Expr::Func { name: first.to_ascii_uppercase(), args, pos });
        }
        let mut parts = vec![first];
        while *self.peek() == Tok::Dot {
            self.next();
            parts.push(self.ident()?.0);
        }
        Ok(Expr::Path { parts, pos })
    }

    /// Conditions joined by AND or by OR. SOQL has no precedence between
    /// them: mixing the two needs parentheses.
    fn cond(&mut self) -> Result<Cond, SfError> {
        let first = self.unary()?;
        let (op, other) = if self.is_kw("AND") {
            ("AND", "OR")
        } else if self.is_kw("OR") {
            ("OR", "AND")
        } else {
            return Ok(first);
        };
        let mut terms = vec![first];
        while self.eat_kw(op) {
            terms.push(self.unary()?);
        }
        if self.is_kw(other) {
            return Err(self.unexpected());
        }
        Ok(if op == "AND" { Cond::And(terms) } else { Cond::Or(terms) })
    }

    fn unary(&mut self) -> Result<Cond, SfError> {
        if self.eat_kw("NOT") {
            return Ok(Cond::Not(Box::new(self.unary()?)));
        }
        if *self.peek() == Tok::LParen {
            self.next();
            let c = self.cond()?;
            self.expect(Tok::RParen)?;
            return Ok(c);
        }
        let lhs = self.expr()?;
        let pos = self.pos();
        if let Tok::Op(op) = *self.peek() {
            self.next();
            let rhs = self.literal()?;
            return Ok(Cond::Cmp { lhs, op, rhs, pos });
        }
        if self.eat_kw("LIKE") {
            let rhs = self.literal()?;
            return Ok(Cond::Cmp { lhs, op: "LIKE", rhs, pos });
        }
        let negated = self.eat_kw("NOT");
        if self.eat_kw("IN") {
            self.expect(Tok::LParen)?;
            let source = if self.is_kw("SELECT") {
                InSource::Query(Box::new(self.query()?))
            } else {
                let mut vals = vec![self.literal()?];
                while *self.peek() == Tok::Comma {
                    self.next();
                    vals.push(self.literal()?);
                }
                InSource::List(vals)
            };
            self.expect(Tok::RParen)?;
            return Ok(Cond::In { lhs, negated, source });
        }
        if negated {
            return Err(self.unexpected());
        }
        let excludes = if self.eat_kw("INCLUDES") {
            false
        } else if self.eat_kw("EXCLUDES") {
            true
        } else {
            return Err(self.unexpected());
        };
        self.expect(Tok::LParen)?;
        let mut values = vec![];
        loop {
            match self.next().tok {
                Tok::Str(s) => values.push(s),
                _ => {
                    self.i -= 1;
                    return Err(self.unexpected());
                }
            }
            if *self.peek() != Tok::Comma {
                break;
            }
            self.next();
        }
        self.expect(Tok::RParen)?;
        Ok(Cond::Includes { lhs, excludes, values, pos })
    }

    fn literal(&mut self) -> Result<Literal, SfError> {
        let t = self.next();
        Ok(match t.tok {
            Tok::Str(s) => Literal::Str(s),
            Tok::Num(n) => Literal::Num(n),
            Tok::Date(d) | Tok::DateTime(d) if chrono::NaiveDate::parse_from_str(&d[..10], "%Y-%m-%d").is_err() => {
                return Err(query_error("MALFORMED_QUERY", self.src, t.pos, &format!("invalid date: {d}")));
            }
            Tok::Date(d) => Literal::Date(d),
            Tok::DateTime(d) => Literal::DateTime(d),
            Tok::Ident(s) if s.eq_ignore_ascii_case("null") => Literal::Null,
            Tok::Ident(s) if s.eq_ignore_ascii_case("true") => Literal::Bool(true),
            Tok::Ident(s) if s.eq_ignore_ascii_case("false") => Literal::Bool(false),
            Tok::Ident(s) => {
                let name = s.to_ascii_uppercase();
                let n = if *self.peek() == Tok::Colon {
                    self.next();
                    Some(self.uint()? as i64)
                } else {
                    None
                };
                // LAST_N_DAYS:n and friends need their n; TODAY and friends take none.
                let takes_n = name.starts_with("N_") || name.contains("_N_");
                if takes_n != n.is_some() {
                    return Err(query_error("MALFORMED_QUERY", self.src, t.pos, &format!("unexpected token: '{s}'")));
                }
                Literal::DateLiteral(name, n)
            }
            _ => {
                self.i -= 1;
                return Err(self.unexpected());
            }
        })
    }
}
