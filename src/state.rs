use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rand::Rng;
use rand::distributions::Alphanumeric;
use rusqlite::Connection;
use serde_json::{Map, Value};

use crate::config::Config;
use crate::ids;
use crate::inspector::Inspector;
use crate::schema::{Catalog, CustomSchema, Source};
use crate::store::{self, Ctx};

pub const ORG_SEQ: u64 = 1;
pub const API_LIMIT: u64 = 15_000;

/// A query's results, served a page at a time by `/query/<locator>-<offset>`.
/// Like Salesforce's, a locator can be read more than once until it expires.
#[derive(Clone)]
pub struct Cursor {
    pub records: Arc<Vec<Value>>,
    pub total_size: usize,
    pub touched: std::time::Instant,
}

/// How long an unused query locator lasts.
const CURSOR_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub struct Inner {
    pub config: Config,
    /// Swapped for a new catalog when the schema is changed in the UI.
    cat: RwLock<Arc<Catalog>>,
    /// Definitions read from SFDX metadata at startup.
    pub metadata: CustomSchema,
    /// Definitions created in the web UI (persisted in the `_schema` table).
    pub(crate) ui_schema: Mutex<CustomSchema>,
    pub db: Mutex<Connection>,
    pub inspector: Inspector,
    pub org_id: String,
    pub default_user: String,
    /// access token -> user id
    pub sessions: Mutex<HashMap<String, String>>,
    pub revoked: Mutex<HashSet<String>>,
    /// OAuth authorization codes and refresh tokens.
    pub grants: crate::oauth::Grants,
    pub cursors: Mutex<HashMap<String, Cursor>>,
    pub api_calls: AtomicU64,
    /// Bulk API jobs.
    pub bulk: crate::bulk::Jobs,
    cursor_seq: AtomicU64,
    /// When the schema last changed, for `If-Modified-Since` on describes.
    pub schema_changed: RwLock<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let mut metadata = CustomSchema::default();
        for dir in &config.metadata {
            let loaded = crate::sfdx::load(dir)?;
            tracing::info!(
                "metadata:           {} ({} objects, {} fields on standard objects)",
                dir.display(),
                loaded.schema.objects.len(),
                loaded.schema.fields.len()
            );
            for s in loaded.skipped {
                tracing::warn!("skipped {s}");
            }
            metadata.objects.extend(loaded.schema.objects);
            metadata.fields.extend(loaded.schema.fields);
            metadata.validation_rules.extend(loaded.schema.validation_rules);
        }
        let path = config.data_dir.as_ref().map(|d| d.join("org.db"));
        let (conn, (ui_schema, cat, default_user)) =
            crate::data::open_org(path.as_deref(), config.restore.as_deref(), |conn| {
                let ui_schema = crate::schema_admin::load_ui_schema(conn)?;
                let cat = build_catalog(&config.schema, &metadata, &ui_schema)?;
                store::sync_tables(conn, &cat)?;
                let mut user_ids = vec![];
                for u in &config.users {
                    user_ids.push(ensure_user(conn, &cat, u)?);
                }
                let default_user = user_ids[0].clone();
                ensure_standard_price_book(conn, &cat, &default_user)?;
                let ctx = Ctx {
                    user_id: default_user.clone(),
                    now: chrono::Utc::now() + chrono::Duration::seconds(config.clock_offset_secs),
                    api_version: crate::ui::API_VERSION.into(),
                };
                crate::data::seed(conn, &config.seed, &cat, &ctx)?;
                Ok((ui_schema, cat, default_user))
            })?;
        let org_id = ids::make("00D", ORG_SEQ);
        for problem in crate::formula::check(&cat) {
            tracing::warn!("{problem}");
        }
        Ok(AppState(Arc::new(Inner {
            config,
            cat: RwLock::new(Arc::new(cat)),
            metadata,
            ui_schema: Mutex::new(ui_schema),
            db: Mutex::new(conn),
            inspector: Inspector::default(),
            org_id,
            default_user,
            sessions: Mutex::default(),
            revoked: Mutex::default(),
            grants: Default::default(),
            cursors: Mutex::default(),
            api_calls: AtomicU64::new(0),
            bulk: Default::default(),
            cursor_seq: AtomicU64::new(0),
            schema_changed: RwLock::new(chrono::Utc::now()),
        })))
    }

    /// The current catalog. Hold on to the `Arc` for the length of a request.
    pub fn cat(&self) -> Arc<Catalog> {
        self.cat.read().unwrap().clone()
    }

    pub(crate) fn set_cat(&self, cat: Catalog) {
        *self.cat.write().unwrap() = Arc::new(cat);
        *self.schema_changed.write().unwrap() = chrono::Utc::now();
    }

    pub fn ctx(&self, user_id: &str, api_version: &str) -> Ctx {
        Ctx {
            user_id: user_id.to_string(),
            now: chrono::Utc::now() + chrono::Duration::seconds(self.config.clock_offset_secs),
            api_version: api_version.to_string(),
        }
    }

    /// A session id shaped like Salesforce's: `<org id>!<random>`.
    pub fn new_session(&self, user_id: &str) -> String {
        let secret: String = rand::thread_rng().sample_iter(&Alphanumeric).take(96).map(char::from).collect();
        let token = format!("{}!AQ{secret}", &self.org_id[..15]);
        self.sessions.lock().unwrap().insert(token.clone(), user_id.to_string());
        token
    }

    /// Which user a bearer token belongs to. Unknown tokens are accepted as
    /// the default user unless sessions are strict; revoked ones never are.
    pub fn user_for_token(&self, token: &str) -> Option<String> {
        if token.is_empty() || self.revoked.lock().unwrap().contains(token) {
            return None;
        }
        match self.sessions.lock().unwrap().get(token) {
            Some(u) => Some(u.clone()),
            None if !self.config.strict_sessions => Some(self.default_user.clone()),
            None => None,
        }
    }

    pub fn user_by_username(&self, username: &str) -> Option<String> {
        let conn = self.db.lock().unwrap();
        conn.query_row("SELECT \"Id\" FROM \"User\" WHERE \"Username\" = ?1 AND \"IsActive\" = 1", [username], |r| {
            r.get(0)
        })
        .ok()
    }

    pub fn store_cursor(&self, cursor: Cursor) -> String {
        let n = self.cursor_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let id = ids::make("01g", n)[..15].to_string();
        let mut cursors = self.cursors.lock().unwrap();
        cursors.retain(|_, c| c.touched.elapsed() < CURSOR_TTL);
        cursors.insert(id.clone(), cursor);
        id
    }

    /// A stored query's results, if its locator hasn't expired.
    pub fn cursor(&self, id: &str) -> Option<Cursor> {
        let mut cursors = self.cursors.lock().unwrap();
        let c = cursors.get_mut(id).filter(|c| c.touched.elapsed() < CURSOR_TTL)?;
        c.touched = std::time::Instant::now();
        Some(c.clone())
    }

    pub fn count_call(&self) -> u64 {
        self.api_calls.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Standard objects, then the config file, SFDX metadata and the UI's definitions.
///
/// Files win over the UI: an object or field made in the UI and later saved
/// into the project's metadata is shadowed by the file's definition (its
/// data stays, in the same table and column).
pub(crate) fn build_catalog(
    config: &CustomSchema,
    metadata: &CustomSchema,
    ui: &CustomSchema,
) -> anyhow::Result<Catalog> {
    let files = [config, metadata];
    let file_object = |name: &str| files.iter().any(|l| l.objects.iter().any(|o| o.name.eq_ignore_ascii_case(name)));
    let file_field = |object: &str, field: &str| {
        files.iter().any(|l| {
            l.objects.iter().any(|o| {
                o.name.eq_ignore_ascii_case(object) && o.fields.iter().any(|f| f.name.eq_ignore_ascii_case(field))
            }) || l
                .fields
                .iter()
                .any(|f| f.object.eq_ignore_ascii_case(object) && f.field.name.eq_ignore_ascii_case(field))
        })
    };
    let mut visible = ui.clone();
    visible.objects.retain(|o| {
        let shadowed = file_object(&o.name);
        if shadowed {
            tracing::debug!("{} is defined in files; ignoring the UI's definition", o.name);
        }
        !shadowed
    });
    visible.fields.retain(|f| !file_field(&f.object, &f.field.name));
    Catalog::build(&[(Source::Config, config), (Source::Metadata, metadata), (Source::Ui, &visible)])
}

/// Every org has a standard price book.
pub(crate) fn ensure_standard_price_book(conn: &Connection, cat: &Catalog, user: &str) -> anyhow::Result<()> {
    let exists: bool =
        conn.query_row("SELECT EXISTS (SELECT 1 FROM \"Pricebook2\" WHERE \"IsStandard\" = 1)", [], |r| r.get(0))?;
    if exists {
        return Ok(());
    }
    let obj = cat.object("Pricebook2").unwrap();
    let mut body = Map::new();
    body.insert("Name".into(), "Standard Price Book".into());
    body.insert("IsActive".into(), true.into());
    let ctx = Ctx { user_id: user.to_string(), now: chrono::Utc::now(), api_version: "62.0".into() };
    let id = store::insert(conn, cat, &ctx, obj, &body)
        .map_err(|e| anyhow::anyhow!("creating the standard price book: {}", e.message))?;
    conn.execute("UPDATE \"Pricebook2\" SET \"IsStandard\" = 1 WHERE \"Id\" = ?1", [&id])?;
    Ok(())
}

fn ensure_user(conn: &Connection, cat: &Catalog, u: &crate::config::UserDef) -> anyhow::Result<String> {
    if let Ok(id) = conn.query_row("SELECT \"Id\" FROM \"User\" WHERE \"Username\" = ?1", [&u.username], |r| r.get(0)) {
        return Ok(id);
    }
    let obj = cat.object("User").unwrap();
    let mut body = Map::new();
    body.insert("Username".into(), u.username.clone().into());
    body.insert("Email".into(), u.email.clone().unwrap_or_else(|| u.username.clone()).into());
    body.insert("FirstName".into(), u.first_name.clone().into());
    body.insert("LastName".into(), u.last_name.clone().into());
    body.insert("Alias".into(), u.username.chars().take(8).collect::<String>().into());
    // The first user creates itself, so its audit fields point at itself.
    let ctx = Ctx { user_id: String::new(), now: chrono::Utc::now(), api_version: "62.0".into() };
    let id = store::insert(conn, cat, &ctx, obj, &body)
        .map_err(|e| anyhow::anyhow!("creating user {}: {}", u.username, e.message))?;
    conn.execute(
        "UPDATE \"User\" SET \"CreatedById\" = coalesce(nullif(\"CreatedById\", ''), ?1), \"LastModifiedById\" = coalesce(nullif(\"LastModifiedById\", ''), ?1) WHERE \"Id\" = ?1",
        [&id],
    )?;
    Ok(id)
}
