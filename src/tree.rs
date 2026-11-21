//! Inserting Salesforce record trees, shared by composite REST requests and startup fixtures.

use rusqlite::Connection;
use serde_json::{Map, Value, json};

use crate::error::SfError;
use crate::schema::{Catalog, FieldType, SObject};
use crate::store::{self, Ctx};

pub(crate) struct Tree<'r> {
    conn: &'r Connection,
    cat: &'r Catalog,
    ctx: &'r Ctx,
    // In insertion order, as required by the composite response.
    pub(crate) ids: Map<String, Value>,
    pub(crate) resolve_refs: bool,
}
impl<'r> Tree<'r> {
    pub(crate) fn new(conn: &'r Connection, cat: &'r Catalog, ctx: &'r Ctx) -> Self {
        Self { conn, cat, ctx, ids: Map::new(), resolve_refs: false }
    }

    /// Insert a record and its children; on failure, the reference and why.
    pub(crate) fn insert(
        &mut self,
        rec: &Value,
        o: &SObject,
        parent: Option<(&str, &str)>,
        depth: usize,
    ) -> Result<(), Value> {
        let fail = |r: &str, e: SfError| json!({ "referenceId": r, "errors": [e.save_result_json()] });
        let rec = rec.as_object().cloned().unwrap_or_default();
        let attrs = rec.get("attributes").cloned().unwrap_or(Value::Null);
        let reference = attrs.get("referenceId").and_then(Value::as_str).unwrap_or_default().to_string();
        if reference.is_empty() {
            return Err(fail(
                "",
                SfError::new("INVALID_INPUT", "Include a reference ID for each record in the request."),
            ));
        }
        if self.ids.contains_key(&reference) {
            return Err(fail(
                &reference,
                SfError::new("INVALID_INPUT", format!("Duplicate ReferenceId provided in the request: {reference}")),
            ));
        }
        let ty = attrs.get("type").and_then(Value::as_str).unwrap_or_default();
        if !ty.eq_ignore_ascii_case(&o.name) {
            return Err(fail(
                &reference,
                SfError::new("INVALID_INPUT", format!("The type of record {reference} must be {}", o.name)),
            ));
        }
        if depth > 5 {
            return Err(fail(&reference, SfError::new("INVALID_INPUT", "A tree can be at most 5 levels deep")));
        }
        let mut fields = Map::new();
        let mut children = vec![];
        for (k, v) in rec {
            if k == "attributes" {
                continue;
            }
            match (v.get("records").and_then(Value::as_array), self.cat.child_relationship(&o.name, &k)) {
                (Some(recs), Some(rel)) => children.push((rel.clone(), recs.clone())),
                (Some(_), None) => {
                    return Err(fail(
                        &reference,
                        SfError::new("INVALID_FIELD", format!("No such child relationship '{k}' on {}", o.name)),
                    ));
                }
                _ => {
                    let mut value = v;
                    if self.resolve_refs
                        && o.field(&k).is_some_and(|f| f.ty == FieldType::Reference)
                        && let Some(key) = value.as_str().and_then(|s| s.strip_prefix('@'))
                    {
                        value = self.ids.get(key).cloned().ok_or_else(|| {
                            fail(
                                &reference,
                                SfError::new("INVALID_INPUT", format!("Unknown fixture reference @{key} in {k}")),
                            )
                        })?;
                    }
                    fields.insert(k, value);
                }
            }
        }
        if let Some((fk, id)) = parent {
            fields.insert(fk.to_string(), json!(id));
        }
        let id = store::insert(self.conn, self.cat, self.ctx, o, &fields).map_err(|e| fail(&reference, e))?;
        self.ids.insert(reference, json!(id));
        for (rel, recs) in children {
            let child = self.cat.object(&rel.child_object).unwrap();
            for r in &recs {
                self.insert(r, child, Some((&rel.field, &id)), depth + 1)?;
            }
        }
        Ok(())
    }
}
