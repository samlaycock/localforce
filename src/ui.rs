//! The web UI, served at `/__localforce/ui/`, and the JSON API behind it
//! under `/__localforce/api/`.
//!
//! The page edits records through the real REST API (with a session from
//! `/__localforce/api/session`), so it sees exactly the validation and errors
//! a client would. This API covers what Salesforce's doesn't: changing the
//! schema, paging through any number of records, the recycle bin, the request
//! log and resetting the org. Like the Salesforce endpoints it has no
//! authentication, but it refuses cross-origin requests, so other websites
//! open in the same browser can't use it.

use std::collections::HashMap;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::SfError;
use crate::routes::Scheme;
use crate::schema::{CustomObjectDef, Derived, FieldDef, FieldType, SObject, Source};
use crate::schema_admin::source_name;
use crate::state::AppState;
use crate::{describe, sfdx, soql, store};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/app.css");
const FAVICON: &str = include_str!("../ui/favicon.svg");

pub const API_VERSION: &str = "62.0";

pub fn router() -> Router<AppState> {
    let api = Router::new()
        .route("/info", get(info))
        .route("/session", post(session))
        .route("/objects", get(objects).post(create_object))
        .route("/objects/{name}", get(object).delete(delete_object))
        .route("/objects/{name}/fields", post(add_field))
        .route("/objects/{name}/fields/{field}", axum::routing::put(update_field).delete(delete_field))
        .route("/objects/{name}/metadata", get(export_metadata))
        .route("/records/{name}", get(records))
        .route("/records/{name}/{id}/related", get(related))
        .route("/records/{name}/{id}/undelete", post(undelete))
        .route("/query", post(query))
        .route("/requests", get(requests).delete(clear_requests))
        .route("/requests/{id}", get(request))
        .route("/backup", get(backup))
        .route("/reset", post(reset))
        .layer(middleware::from_fn(same_origin));
    Router::new()
        .route("/", get(|| async { Redirect::to("/__localforce/ui/") }))
        .route("/__localforce/ui", get(|| async { Redirect::permanent("/__localforce/ui/") }))
        .route("/__localforce/ui/", get(|| async { asset("index.html") }))
        .route("/__localforce/ui/{name}", get(|Path(name): Path<String>| async move { asset(&name) }))
        .nest("/__localforce/api", api)
}

/// An embedded file of the page. For working on the UI, `LOCALFORCE_UI_DIR`
/// serves them from a directory instead, so changes show without a rebuild.
fn asset(name: &str) -> Response {
    let (embedded, content_type) = match name {
        "index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        "app.js" => (APP_JS, "text/javascript; charset=utf-8"),
        "app.css" => (APP_CSS, "text/css; charset=utf-8"),
        "favicon.svg" => (FAVICON, "image/svg+xml"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let body = std::env::var_os("LOCALFORCE_UI_DIR")
        .and_then(|dir| std::fs::read_to_string(std::path::Path::new(&dir).join(name)).ok())
        .unwrap_or_else(|| embedded.to_owned());
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// Refuse requests from other origins (a page on another site posting here).
async fn same_origin(headers: HeaderMap, req: Request, next: Next) -> Response {
    let cross_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|v| v == "cross-site");
    let origin_mismatch = match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
        (Some(o), Some(h)) => {
            let o = o.to_str().unwrap_or("");
            let host = h.to_str().unwrap_or("");
            !(o.ends_with(&format!("://{host}")))
        }
        _ => false,
    };
    if cross_site || origin_mismatch {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "cross-origin requests are not allowed" })))
            .into_response();
    }
    next.run(req).await
}

fn bad(e: impl std::fmt::Display) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response()
}

fn sf(e: SfError) -> Response {
    (e.status(), Json(json!({ "error": e.message.trim(), "errorCode": e.code }))).into_response()
}

async fn info(State(st): State<AppState>, Extension(scheme): Extension<Scheme>, headers: HeaderMap) -> Json<Value> {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    let hostname = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let users: Vec<Value> = {
        let conn = st.db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT \"Id\", \"Username\", \"Name\" FROM \"User\" ORDER BY rowid").unwrap();
        stmt.query_map([], |r| {
            Ok(json!({ "id": r.get::<_, String>(0)?, "username": r.get::<_, String>(1)?, "name": r.get::<_, Option<String>>(2)? }))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    };
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "apiVersion": API_VERSION,
        "orgId": st.org_id,
        "httpUrl": format!("http://{hostname}:{}", st.config.port),
        "httpsUrl": format!("https://{hostname}:{}", st.config.tls_port),
        "scheme": scheme.0,
        "certificate": st.config.cert_dir.join("localforce-cert.pem"),
        "persistent": st.config.data_dir.is_some(),
        "dataDir": st.config.data_dir,
        "metadata": st.config.metadata,
        "defaultUser": st.default_user,
        "strictSessions": st.config.strict_sessions,
        "users": users,
    }))
}

#[derive(Deserialize)]
struct SessionReq {
    user: Option<String>,
}

/// A session for the page to call the REST API with.
async fn session(State(st): State<AppState>, Json(body): Json<SessionReq>) -> Json<Value> {
    let user = body.user.unwrap_or_else(|| st.default_user.clone());
    Json(json!({ "accessToken": st.new_session(&user), "userId": user }))
}

fn counts(st: &AppState, obj: &SObject) -> (i64, i64) {
    let conn = st.db.lock().unwrap();
    conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(\"IsDeleted\" = 0), 0), COALESCE(SUM(\"IsDeleted\" = 1), 0) FROM {}",
            store::qi(&obj.name)
        ),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((0, 0))
}

async fn objects(State(st): State<AppState>) -> Json<Value> {
    let cat = st.cat();
    let list: Vec<Value> = cat
        .objects
        .iter()
        .map(|o| {
            let (live, deleted) = counts(&st, o);
            json!({
                "name": o.name,
                "label": o.label,
                "labelPlural": o.label_plural,
                "custom": o.custom,
                "keyPrefix": o.key_prefix,
                "source": o.source,
                "nameField": o.name_field().map(|f| f.name.clone()),
                "count": live,
                "deletedCount": deleted,
            })
        })
        .collect();
    Json(json!(list))
}

fn field_json(cat: &crate::schema::Catalog, o: &SObject, f: &crate::schema::Field) -> Value {
    let kind = match &f.derived {
        Derived::System => "system",
        Derived::FullName | Derived::Address(_) => "compound",
        Derived::AutoNumber(_) => "autonumber",
        Derived::Formula(_) | Derived::Rollup(_) => "formula",
        Derived::Logic => "system",
        Derived::None => "",
    };
    let parent_name_field =
        f.reference_to.as_ref().and_then(|t| cat.object(t)).and_then(|p| p.name_field()).map(|nf| nf.name.clone());
    json!({
        "name": f.name,
        "label": f.label,
        "type": f.ty,
        "describeType": f.ty.describe_name(),
        "kind": kind,
        "formula": match &f.derived {
            Derived::Formula(s) => Some(s.clone()),
            Derived::Rollup(r) => Some(match &r.field {
                Some(field) => format!("roll-up summary: {}({}.{field})", r.op.to_ascii_uppercase(), r.object),
                None => format!("roll-up summary: COUNT({})", r.object),
            }),
            _ => None,
        },
        "autoNumberFormat": match &f.derived { Derived::AutoNumber(s) => Some(s.clone()), _ => None },
        "description": f.description,
        "length": f.length,
        "precision": f.precision,
        "scale": f.scale,
        "required": f.required,
        "createable": f.createable,
        "updateable": f.updateable,
        "custom": f.custom,
        "source": f.source,
        // Name, Id, OwnerId and the audit fields belong to the object, not the UI's field list.
        "editable": f.source == Source::Ui && f.custom,
        "nameField": o.name_field().is_some_and(|n| n.name == f.name),
        "referenceTo": f.reference_to,
        "referenceNameField": parent_name_field,
        "relationshipName": f.relationship_name,
        "childRelationshipName": f.child_relationship_name,
        "masterDetail": f.master_detail,
        "restrictDelete": f.restrict_delete,
        "values": f.picklist,
        "restricted": f.restricted,
        "default": f.default,
        "defaultFormula": f.default_formula,
        "externalId": f.external_id,
        "unique": f.unique,
    })
}

async fn object(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let cat = st.cat();
    let Some(o) = cat.object(&name) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "no such object" }))).into_response();
    };
    let (live, deleted) = counts(&st, o);
    Json(json!({
        "name": o.name,
        "label": o.label,
        "labelPlural": o.label_plural,
        "custom": o.custom,
        "keyPrefix": o.key_prefix,
        "source": o.source,
        "sourceName": source_name(o.source),
        "editable": o.source == Source::Ui,
        "description": o.description,
        "nameField": o.name_field().map(|f| f.name.clone()),
        "count": live,
        "deletedCount": deleted,
        "fields": o.fields.iter().map(|f| field_json(&cat, o, f)).collect::<Vec<_>>(),
        "childRelationships": cat.children_of(&o.name).iter().map(|c| json!({
            "name": c.name, "childObject": c.child_object, "field": c.field, "cascadeDelete": c.cascade_delete,
        })).collect::<Vec<_>>(),
        "describe": describe::sobject(API_VERSION, &cat, o),
    }))
    .into_response()
}

async fn create_object(State(st): State<AppState>, Json(def): Json<CustomObjectDef>) -> Response {
    match st.create_object(def) {
        Ok(name) => (StatusCode::CREATED, Json(json!({ "name": name }))).into_response(),
        Err(e) => bad(e),
    }
}

async fn delete_object(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.delete_object(&name) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => bad(e),
    }
}

async fn add_field(State(st): State<AppState>, Path(name): Path<String>, Json(def): Json<FieldDef>) -> Response {
    match st.add_field(&name, def) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => bad(e),
    }
}

async fn update_field(
    State(st): State<AppState>,
    Path((name, field)): Path<(String, String)>,
    Json(def): Json<FieldDef>,
) -> Response {
    if !def.name.eq_ignore_ascii_case(&field) {
        return bad("a field's API name can't be changed");
    }
    match st.update_field(&name, def) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => bad(e),
    }
}

async fn delete_field(State(st): State<AppState>, Path((name, field)): Path<(String, String)>) -> Response {
    match st.delete_field(&name, &field) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => bad(e),
    }
}

async fn export_metadata(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    let cat = st.cat();
    let Some(o) = cat.object(&name) else { return bad("no such object") };
    let files: Vec<Value> =
        sfdx::export(o).into_iter().map(|(path, content)| json!({ "path": path, "content": content })).collect();
    Json(json!({ "root": "force-app/main/default", "files": files })).into_response()
}

/// A SOQL string literal.
fn quote(s: &str, like: bool) -> String {
    let mut out = s.replace('\\', "\\\\").replace('\'', "\\'");
    if like {
        out = out.replace('%', "\\%").replace('_', "\\_");
    }
    format!("'{out}'")
}

/// One page of an object's records for the table view, by building SOQL
/// (so relationship columns such as `Account.Name` work) without SOQL's
/// 2,000-row OFFSET cap.
async fn records(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let cat = st.cat();
    let Some(o) = cat.object(&name) else { return bad("no such object") };
    let fields: Vec<String> = q
        .get("fields")
        .map(|f| f.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let mut select = vec!["Id".to_string()];
    for f in fields {
        if !select.iter().any(|s| s.eq_ignore_ascii_case(&f)) {
            select.push(f);
        }
    }
    let deleted = q.get("deleted").is_some_and(|v| v == "1");
    let mut wh = vec![];
    if deleted {
        wh.push("IsDeleted = true".to_string());
    }
    if let Some(search) = q.get("q").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let mut ors = vec![];
        if crate::ids::to18(search).is_some() {
            ors.push(format!("Id = {}", quote(search, false)));
        }
        let pattern = format!("'%{}%'", &quote(search, true)[1..quote(search, true).len() - 1]);
        for f in o
            .fields
            .iter()
            .filter(|f| {
                matches!(
                    f.ty,
                    FieldType::String | FieldType::Email | FieldType::Phone | FieldType::Url | FieldType::Picklist
                ) && !f.is_system()
            })
            .take(8)
        {
            ors.push(format!("{} LIKE {pattern}", f.name));
        }
        if ors.is_empty() {
            return Json(json!({ "totalSize": 0, "records": [] })).into_response();
        }
        wh.push(format!("({})", ors.join(" OR ")));
    }
    // Related lists: the children of one parent record.
    if let (Some(field), Some(value)) = (q.get("field"), q.get("value")) {
        let Some(f) = o.field(field).filter(|f| f.ty == FieldType::Reference) else {
            return bad("filter field must be a lookup");
        };
        wh.push(format!("{} = {}", f.name, quote(value, false)));
    }
    let where_ = if wh.is_empty() { String::new() } else { format!(" WHERE {}", wh.join(" AND ")) };
    let order = match q.get("order").filter(|s| !s.is_empty()) {
        Some(col) => {
            let dir = if q.get("dir").is_some_and(|d| d == "desc") { "DESC NULLS LAST" } else { "ASC NULLS FIRST" };
            format!(" ORDER BY {col} {dir}, Id")
        }
        None => " ORDER BY CreatedDate DESC, Id DESC".to_string(),
    };
    let limit: u64 = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50).min(2000);
    let offset: u64 = q.get("offset").and_then(|l| l.parse().ok()).unwrap_or(0);
    let soql_text =
        format!("SELECT {} FROM {}{where_}{order} LIMIT {limit} OFFSET {offset}", select.join(", "), o.name);
    let count_text = format!("SELECT COUNT() FROM {}{where_}", o.name);

    let ctx = st.ctx(&st.default_user, API_VERSION);
    let conn = st.db.lock().unwrap();
    let total = match soql::exec::run_with_offset_limit(&conn, &cat, &ctx, &count_text, deleted, u64::MAX) {
        Ok(out) => out.total_size,
        Err(e) => return sf(e),
    };
    match soql::exec::run_with_offset_limit(&conn, &cat, &ctx, &soql_text, deleted, u64::MAX) {
        Ok(out) => Json(json!({ "totalSize": total, "records": out.records, "soql": soql_text })).into_response(),
        Err(e) => sf(e),
    }
}

/// Related lists for a record: each child relationship with a count.
async fn related(State(st): State<AppState>, Path((name, id)): Path<(String, String)>) -> Response {
    let cat = st.cat();
    let Some(o) = cat.object(&name) else { return bad("no such object") };
    let Some(id) = crate::ids::to18(&id) else { return bad("malformed id") };
    let conn = st.db.lock().unwrap();
    let list: Vec<Value> = cat
        .children_of(&o.name)
        .iter()
        .map(|c| {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {} WHERE {} = ?1 AND \"IsDeleted\" = 0",
                        store::qi(&c.child_object),
                        store::qi(&c.field)
                    ),
                    [&id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            let child = cat.object(&c.child_object).unwrap();
            json!({
                "relationship": c.name,
                "childObject": c.child_object,
                "childLabelPlural": child.label_plural,
                "field": c.field,
                "count": n,
                "nameField": child.name_field().map(|f| f.name.clone()),
            })
        })
        .collect();
    Json(json!(list)).into_response()
}

async fn undelete(State(st): State<AppState>, Path((name, id)): Path<(String, String)>) -> Response {
    let cat = st.cat();
    let Some(o) = cat.object(&name) else { return bad("no such object") };
    let ctx = st.ctx(&st.default_user, API_VERSION);
    let conn = st.db.lock().unwrap();
    match store::undelete(&conn, &cat, &ctx, o, &id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => sf(e),
    }
}

#[derive(Deserialize)]
struct QueryReq {
    soql: String,
    #[serde(default)]
    all: bool,
}

/// The SOQL console. Same engine and limits as `/query`, but returns every
/// row (up to 50,000) instead of 2,000-record pages.
async fn query(State(st): State<AppState>, Json(body): Json<QueryReq>) -> Response {
    let cat = st.cat();
    let ctx = st.ctx(&st.default_user, API_VERSION);
    let conn = st.db.lock().unwrap();
    let start = std::time::Instant::now();
    match soql::run(&conn, &cat, &ctx, &body.soql, body.all) {
        Ok(mut out) => {
            let truncated = out.records.len() > 50_000;
            out.records.truncate(50_000);
            Json(json!({
                "totalSize": out.total_size,
                "done": true,
                "records": out.records,
                "truncated": truncated,
                "ms": (start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
            }))
            .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!([e.rest_json()]))).into_response(),
    }
}

async fn requests(State(st): State<AppState>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let after = q.get("after").and_then(|a| a.parse().ok()).unwrap_or(0);
    let list: Vec<Value> = st
        .inspector
        .list(after)
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id, "time": e.time, "method": e.method, "path": e.path, "status": e.status,
                "durationMs": e.duration_ms, "ui": e.ui, "errorCode": e.error_code,
            })
        })
        .collect();
    Json(json!(list))
}

async fn request(State(st): State<AppState>, Path(id): Path<u64>) -> Response {
    match st.inspector.get(id) {
        Some(e) => Json(json!(e)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn clear_requests(State(st): State<AppState>) -> StatusCode {
    st.inspector.clear();
    StatusCode::NO_CONTENT
}

async fn backup(State(st): State<AppState>) -> Response {
    let result = tokio::task::spawn_blocking(move || crate::data::backup(&st.db.lock().unwrap())).await;
    match result.map_err(anyhow::Error::from).and_then(|r| r) {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
            (
                [
                    (header::CONTENT_TYPE, "application/octet-stream"),
                    (header::CONTENT_DISPOSITION, "attachment; filename=localforce.db"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                axum::body::Body::from_stream(stream),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

async fn reset(State(st): State<AppState>) -> Response {
    let cat = st.cat();
    let conn = st.db.lock().unwrap();
    st.cursors.lock().unwrap().clear();
    if let Err(e) = store::reset(&conn, &cat) {
        return sf(e);
    }
    match crate::state::ensure_standard_price_book(&conn, &cat, &st.default_user) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
