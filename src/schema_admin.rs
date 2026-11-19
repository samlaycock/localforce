//! Changing the schema at runtime, from the web UI.
//!
//! Objects and fields created here are stored in the org database's
//! `_schema` table (in memory unless `--data-dir` is set), layered over the
//! config file and SFDX metadata. Only definitions made here can be edited
//! or deleted here: the others belong to files.

use rusqlite::Connection;

use crate::schema::{Catalog, CustomFieldOnStandard, CustomObjectDef, CustomSchema, FieldDef, Source, derive_prefix};
use crate::state::{AppState, build_catalog};
use crate::store::{self, qi};

pub fn load_ui_schema(conn: &Connection) -> anyhow::Result<CustomSchema> {
    let mut schema = CustomSchema::default();
    let mut stmt = conn.prepare("SELECT kind, def FROM _schema ORDER BY rowid")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (kind, def) = row?;
        match kind.as_str() {
            "object" => schema.objects.push(serde_json::from_str(&def)?),
            "field" => schema.fields.push(serde_json::from_str(&def)?),
            _ => {}
        }
    }
    Ok(schema)
}

impl AppState {
    /// Apply a change to the UI's schema layer: rebuild the catalog to check
    /// it, then persist the definition and alter the tables in one transaction.
    fn change_schema(
        &self,
        edit: impl FnOnce(&mut CustomSchema, &Catalog) -> anyhow::Result<()>,
        persist: impl FnOnce(&Connection, &Catalog) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let conn = self.db.lock().unwrap();
        let mut ui = self.ui_schema.lock().unwrap();
        let current = self.cat();
        let mut next = ui.clone();
        edit(&mut next, &current)?;
        let cat = build_catalog(&self.config.schema, &self.metadata, &next)?;
        conn.execute_batch("SAVEPOINT lf_schema")?;
        let result = persist(&conn, &cat).and_then(|_| store::sync_tables(&conn, &cat));
        match result {
            Ok(()) => conn.execute_batch("RELEASE lf_schema")?,
            Err(e) => {
                conn.execute_batch("ROLLBACK TO lf_schema; RELEASE lf_schema")?;
                return Err(e);
            }
        }
        *ui = next;
        self.set_cat(cat);
        // Query locators hold records shaped by the old schema.
        self.cursors.lock().unwrap().clear();
        Ok(())
    }

    pub fn create_object(&self, mut def: CustomObjectDef) -> anyhow::Result<String> {
        let current = self.cat();
        anyhow::ensure!(current.object(&def.name).is_none(), "an object named {} already exists", def.name);
        let taken: Vec<String> = current.objects.iter().map(|o| o.key_prefix.clone()).collect();
        // A fixed prefix, so record ids stay valid as other objects come and go.
        def.key_prefix = Some(derive_prefix(&def.name, &taken));
        let name = def.name.clone();
        // Fields are stored separately, so each can be edited on its own.
        let fields: Vec<CustomFieldOnStandard> = std::mem::take(&mut def.fields)
            .into_iter()
            .map(|field| CustomFieldOnStandard { object: name.clone(), field })
            .collect();
        let object_json = serde_json::to_string(&def)?;
        let field_rows = fields
            .iter()
            .map(|f| Ok((f.field.name.clone(), serde_json::to_string(f)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.change_schema(
            |ui, _| {
                ui.objects.push(def);
                ui.fields.extend(fields);
                Ok(())
            },
            |conn, _| {
                save(conn, "object", &name, &name, &object_json)?;
                for (field, json) in &field_rows {
                    save(conn, "field", &name, field, json)?;
                }
                Ok(())
            },
        )?;
        Ok(name)
    }

    pub fn delete_object(&self, name: &str) -> anyhow::Result<()> {
        let current = self.cat();
        let obj = current.object(name).ok_or_else(|| anyhow::anyhow!("no object named {name}"))?;
        anyhow::ensure!(
            obj.source == Source::Ui,
            "{} is defined in {}, so it can't be deleted here",
            obj.name,
            source_name(obj.source)
        );
        let api_name = obj.name.clone();
        let referrers: Vec<String> = current
            .objects
            .iter()
            .filter(|o| !o.name.eq_ignore_ascii_case(&api_name))
            .flat_map(|o| o.fields.iter().map(move |f| (o, f)))
            .filter(|(_, f)| f.reference_to.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(&api_name)))
            .map(|(o, f)| format!("{}.{}", o.name, f.name))
            .collect();
        anyhow::ensure!(
            referrers.is_empty(),
            "{api_name} is referenced by {}; delete those fields first",
            referrers.join(", ")
        );
        self.change_schema(
            |ui, _| {
                ui.objects.retain(|o| !o.name.eq_ignore_ascii_case(&api_name));
                ui.fields.retain(|f| !f.object.eq_ignore_ascii_case(&api_name));
                Ok(())
            },
            |conn, _| {
                conn.execute("DELETE FROM _schema WHERE object = ?1", [&api_name])?;
                conn.execute(&format!("DROP TABLE IF EXISTS {}", qi(&api_name)), [])?;
                conn.execute("DELETE FROM _meta WHERE k LIKE ?1", [format!("auto:{api_name}.%")])?;
                Ok(())
            },
        )
    }

    pub fn add_field(&self, object: &str, field: FieldDef) -> anyhow::Result<()> {
        let current = self.cat();
        let obj = current.object(object).ok_or_else(|| anyhow::anyhow!("no object named {object}"))?;
        anyhow::ensure!(obj.field(&field.name).is_none(), "{} already has a field named {}", obj.name, field.name);
        if let Some(src) = &field.formula {
            crate::formula::check_formula(&current, obj, src).map_err(|e| anyhow::anyhow!("formula: {e}"))?;
        }
        check_default(&current, obj, &field)?;
        let entry = CustomFieldOnStandard { object: obj.name.clone(), field };
        let json = serde_json::to_string(&entry)?;
        let (obj_name, field_name) = (entry.object.clone(), entry.field.name.clone());
        self.change_schema(
            |ui, _| {
                ui.fields.push(entry);
                Ok(())
            },
            |conn, _| save(conn, "field", &obj_name, &field_name, &json),
        )
    }

    pub fn update_field(&self, object: &str, field: FieldDef) -> anyhow::Result<()> {
        let current = self.cat();
        let obj = current.object(object).ok_or_else(|| anyhow::anyhow!("no object named {object}"))?;
        let existing =
            obj.field(&field.name).ok_or_else(|| anyhow::anyhow!("{} has no field {}", obj.name, field.name))?;
        anyhow::ensure!(existing.custom, "{}.{} is a standard field and can't be edited", obj.name, existing.name);
        anyhow::ensure!(
            existing.source == Source::Ui,
            "{}.{} is defined in {}, so it can't be edited here",
            obj.name,
            existing.name,
            source_name(existing.source)
        );
        let probe = field.to_field()?;
        if let Some(src) = &field.formula {
            crate::formula::check_formula(&current, obj, src).map_err(|e| anyhow::anyhow!("formula: {e}"))?;
        }
        check_default(&current, obj, &field)?;
        anyhow::ensure!(
            probe.ty.sql_type() == existing.ty.sql_type() && probe.reference_to == existing.reference_to,
            "a field's type can't be changed; delete it and create a new one"
        );
        let entry = CustomFieldOnStandard { object: obj.name.clone(), field };
        let json = serde_json::to_string(&entry)?;
        let (obj_name, field_name) = (entry.object.clone(), entry.field.name.clone());
        self.change_schema(
            |ui, _| {
                let slot = ui
                    .fields
                    .iter_mut()
                    .find(|f| {
                        f.object.eq_ignore_ascii_case(&obj_name) && f.field.name.eq_ignore_ascii_case(&field_name)
                    })
                    .ok_or_else(|| anyhow::anyhow!("{obj_name}.{field_name} wasn't created in the UI"))?;
                *slot = entry;
                Ok(())
            },
            |conn, _| save(conn, "field", &obj_name, &field_name, &json),
        )
    }

    pub fn delete_field(&self, object: &str, name: &str) -> anyhow::Result<()> {
        let current = self.cat();
        let obj = current.object(object).ok_or_else(|| anyhow::anyhow!("no object named {object}"))?;
        let f = obj.field(name).ok_or_else(|| anyhow::anyhow!("{} has no field {name}", obj.name))?;
        anyhow::ensure!(f.custom, "{}.{} is a standard field and can't be deleted", obj.name, f.name);
        anyhow::ensure!(
            f.source == Source::Ui,
            "{}.{} is defined in {}, so it can't be deleted here",
            obj.name,
            f.name,
            source_name(f.source)
        );
        let (obj_name, field_name) = (obj.name.clone(), f.name.clone());
        self.change_schema(
            |ui, _| {
                ui.fields.retain(|x| {
                    !(x.object.eq_ignore_ascii_case(&obj_name) && x.field.name.eq_ignore_ascii_case(&field_name))
                });
                Ok(())
            },
            |conn, _| {
                conn.execute(
                    "DELETE FROM _schema WHERE kind = 'field' AND object = ?1 AND name = ?2",
                    [&obj_name, &field_name],
                )?;
                conn.execute(&format!("ALTER TABLE {} DROP COLUMN {}", qi(&obj_name), qi(&field_name)), [])?;
                Ok(())
            },
        )
    }
}

fn save(conn: &Connection, kind: &str, object: &str, name: &str, def: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO _schema (kind, object, name, def) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (kind, object, name) DO UPDATE SET def = excluded.def",
        [kind, object, name, def],
    )?;
    Ok(())
}

pub fn source_name(s: Source) -> &'static str {
    crate::schema::source_label(s)
}

/// A field's default-value formula must compile.
fn check_default(cat: &crate::schema::Catalog, obj: &crate::schema::SObject, field: &FieldDef) -> anyhow::Result<()> {
    let f = field.to_field()?;
    if let Some(src) = &f.default_formula {
        crate::formula::check_default(cat, obj, &f, src).map_err(|e| anyhow::anyhow!("default value: {e}"))?;
    }
    Ok(())
}
