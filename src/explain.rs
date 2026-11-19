//! Query plans (`/query?explain=`). localforce has no optimizer, so the plans
//! follow Salesforce's rules of thumb: a filter on an indexed field (Id, Name,
//! OwnerId, lookups, audit dates, unique and external id fields) can lead with
//! an index; anything else is a table scan, with a note for each unindexed
//! filter field.

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::error::{SfError, SfResult};
use crate::schema::{Catalog, FieldType, SObject};
use crate::soql::parse::{Cond, Expr, Literal, Query};
use crate::store::Ctx;

fn indexed(o: &SObject, field: &str) -> bool {
    let Some(f) = o.field(field) else { return false };
    matches!(f.name.as_str(), "Id" | "Name" | "OwnerId" | "CreatedDate" | "LastModifiedDate" | "SystemModstamp")
        || f.ty == FieldType::Reference
        || f.external_id
        || f.unique
        || (f.name == "Email" && matches!(o.name.as_str(), "Contact" | "Lead"))
}

/// The fields of the top-level `AND`ed filters, and whether each can use an index.
fn filters(o: &SObject, q: &Query) -> Vec<(String, bool)> {
    fn field(o: &SObject, q: &Query, e: &Expr) -> Option<String> {
        let Expr::Path { parts, .. } = e else { return None };
        let parts: Vec<&String> = match (&q.alias, parts.as_slice()) {
            (Some(a), [first, rest @ ..]) if first.eq_ignore_ascii_case(a) => rest.iter().collect(),
            (_, [first, rest @ ..]) if first.eq_ignore_ascii_case(&o.name) && !rest.is_empty() => rest.iter().collect(),
            _ => parts.iter().collect(),
        };
        match parts.as_slice() {
            [f] => o.field(f).map(|f| f.name.clone()),
            _ => None,
        }
    }
    let conds: Vec<&Cond> = match &q.where_ {
        Some(Cond::And(cs)) => cs.iter().collect(),
        Some(c) => vec![c],
        None => vec![],
    };
    conds
        .into_iter()
        .filter_map(|c| match c {
            Cond::Cmp { lhs, op, rhs, .. } => {
                let f = field(o, q, lhs)?;
                let leading_wildcard = matches!(rhs, Literal::Str(s) if s.starts_with('%'));
                let usable = !matches!(*op, "!=" | "<>") && !leading_wildcard && !matches!(rhs, Literal::Null);
                Some((f.clone(), usable && indexed(o, &f)))
            }
            Cond::In { lhs, negated, .. } => {
                let f = field(o, q, lhs)?;
                Some((f.clone(), !negated && indexed(o, &f)))
            }
            Cond::Includes { lhs, .. } => field(o, q, lhs).map(|f| (f, false)),
            _ => None,
        })
        .collect()
}

fn round(n: f64) -> f64 {
    (n * 1e10).round() / 1e10
}

/// Plans for one query, cheapest first.
pub fn plans(conn: &Connection, cat: &Catalog, ctx: &Ctx, soql: &str) -> SfResult<Value> {
    let q = crate::soql::parse::parse(soql)?;
    let o = cat
        .object(&q.from)
        .ok_or_else(|| SfError::new("INVALID_TYPE", format!("sObject type '{}' is not supported.", q.from)))?;
    // Run it, both to validate it and to count what it returns.
    let returned = crate::soql::run(conn, cat, ctx, soql, false)?.total_size;
    let total = crate::soql::run(conn, cat, ctx, &format!("SELECT COUNT() FROM {}", o.name), false)?.total_size;
    let fs = filters(o, &q);
    let unindexed: Vec<&str> = fs.iter().filter(|(_, i)| !i).map(|(f, _)| f.as_str()).collect();
    let note = |fields: Vec<&str>| {
        json!({
            "description": "Not considering filter for optimization because unindexed",
            "fields": fields,
            "tableEnumOrId": o.name,
        })
    };
    let scan_cost = round(0.65 + total as f64 / 1_000_000.0);
    let mut out = vec![];
    for (f, _) in fs.iter().filter(|(_, i)| *i) {
        let selectivity = if total == 0 { 0.0 } else { returned as f64 / total as f64 };
        out.push(json!({
            "cardinality": returned,
            "fields": [f],
            "leadingOperationType": "Index",
            "notes": unindexed.iter().map(|u| note(vec![u])).collect::<Vec<_>>(),
            "relativeCost": round(selectivity * scan_cost * 0.5),
            "sobjectCardinality": total,
            "sobjectType": o.name,
        }));
    }
    let mut scan_notes: Vec<Value> = unindexed.iter().map(|u| note(vec![u])).collect();
    scan_notes.push(note(vec!["IsDeleted"]));
    out.push(json!({
        "cardinality": returned,
        "fields": [],
        "leadingOperationType": "TableScan",
        "notes": scan_notes,
        "relativeCost": scan_cost,
        "sobjectCardinality": total,
        "sobjectType": o.name,
    }));
    out.sort_by(|a, b| a["relativeCost"].as_f64().partial_cmp(&b["relativeCost"].as_f64()).unwrap());
    Ok(json!({ "plans": out, "sourceQuery": soql }))
}
