//! Startup fixtures and self-contained SQLite backups for test orgs.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Deserialize;
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::schema::Catalog;
use crate::store::{self, Ctx};
use crate::tree::Tree;

pub(crate) fn seed(conn: &Connection, paths: &[PathBuf], cat: &Catalog, ctx: &Ctx) -> anyhow::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if conn.query_row("SELECT EXISTS(SELECT 1 FROM _meta WHERE k = 'seeded')", [], |r| r.get::<_, bool>(0))? {
        return Ok(());
    }

    let mut tree = Tree::new(conn, cat, ctx);
    tree.resolve_refs = true;
    for path in paths {
        load_fixture(path, cat, &mut tree).with_context(|| format!("loading fixture {}", path.display()))?;
    }
    let count = tree.ids.len();
    conn.execute("INSERT INTO _meta (k, v) VALUES ('seeded', 1)", [])?;
    tracing::info!("loaded {count} fixture records");
    Ok(())
}

fn load_fixture(path: &Path, cat: &Catalog, tree: &mut Tree<'_>) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Fixture {
        records: Vec<Value>,
    }
    let fixture: Fixture = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    for (i, record) in fixture.records.iter().enumerate() {
        let ty = record["attributes"]["type"]
            .as_str()
            .with_context(|| format!("records[{i}] is missing attributes.type"))?;
        let obj = cat.object(ty).with_context(|| format!("records[{i}] has unknown object {ty}"))?;
        tree.insert(record, obj, None, 1).map_err(|err| anyhow::anyhow!("records[{i}]: {err}"))?;
    }
    Ok(())
}

/// VACUUM INTO includes committed WAL pages and produces a standalone database.
pub(crate) fn backup(conn: &Connection) -> anyhow::Result<File> {
    let temp = NamedTempFile::new()?;
    conn.execute("VACUUM main INTO ?1", [temp.path().to_str().context("non-UTF-8 temporary path")?])?;
    Ok(temp.into_file())
}

fn restore(source: &Path, conn: &mut Connection) -> anyhow::Result<()> {
    let src = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let result = Backup::new(&src, conn)?.step(-1)?;
    ensure!(matches!(result, StepResult::Done), "could not finish copying the snapshot: {result:?}");
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    ensure!(check == "ok", "database integrity check failed: {check}");
    conn.query_row("SELECT v FROM _meta WHERE k = 'seq'", [], |r| r.get::<_, i64>(0))
        .context("missing Localforce record sequence")?;
    Ok(())
}

/// Stage disk restores beside the destination until the copied org has passed normal startup.
pub(crate) fn open_org<T>(
    path: Option<&Path>,
    source: Option<&Path>,
    initialize: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> anyhow::Result<(Connection, T)> {
    let staged = match (path, source) {
        (Some(path), Some(_)) => {
            ensure!(!path.try_exists()?, "restore needs a fresh database at {}", path.display());
            Some(NamedTempFile::new_in(path.parent().context("missing data directory")?)?)
        }
        _ => None,
    };
    let mut conn = store::open(staged.as_ref().map(|t| t.path()).or(path))?;
    if let Some(source) = source {
        restore(source, &mut conn).with_context(|| format!("restoring {}", source.display()))?;
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let org = initialize(&tx)?;
    tx.commit()?;
    if let (Some(temp), Some(path)) = (staged, path) {
        conn.close().map_err(|(_, err)| err)?;
        temp.persist_noclobber(path).with_context(|| format!("publishing restored database at {}", path.display()))?;
        conn = store::open(Some(path))?;
    }
    Ok((conn, org))
}
