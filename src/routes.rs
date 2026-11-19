//! HTTP routes: identity, the SOAP login call, and the REST API (OAuth 2.0 is in `oauth`).

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};

use crate::error::SfError;
use crate::rest::{self, Req};
use crate::state::{API_LIMIT, AppState};

/// Which listener a request came in on, for building `instance_url`.
#[derive(Clone, Copy)]
pub struct Scheme(pub &'static str);

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/services/oauth2/token", post(crate::oauth::token))
        .route("/services/oauth2/authorize", get(crate::oauth::authorize).post(crate::oauth::authorize_choice))
        .route("/services/oauth2/userinfo", get(userinfo))
        .route("/services/oauth2/revoke", post(revoke).get(revoke))
        .route("/id/{org}/{user}", get(identity))
        .route("/services/Soap/u/{version}", post(soap))
        .route("/services/Soap/u/{version}/{org}", post(soap))
        .route("/services/Soap/m/{version}", post(metadata_soap))
        .route("/services/Soap/m/{version}/{org}", post(metadata_soap))
        .route("/services/data", get(|| async { Json(rest::versions()) }))
        .route("/services/data/", get(|| async { Json(rest::versions()) }))
        .route("/services/data/{version}", any(data_root))
        .route("/services/data/{version}/", any(data_root))
        .route("/services/data/{version}/{*rest}", any(data))
        .route(
            "/services/data/{version}/jobs/{*rest}",
            any(crate::bulk::v2).layer(DefaultBodyLimit::max(crate::bulk::MAX_UPLOAD)),
        )
        .route(
            "/services/async/{version}/{*rest}",
            any(crate::bulk::v1).layer(DefaultBodyLimit::max(crate::bulk::MAX_UPLOAD)),
        )
        // Apex REST: there's no Apex, so every class is missing.
        .route("/services/apexrest/{*rest}", any(apex_rest))
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::inspector::record))
        // Like Salesforce, accept gzip request bodies and gzip responses when asked
        // (outside the inspector, so the API log shows them uncompressed).
        .layer(tower_http::decompression::RequestDecompressionLayer::new())
        .layer(tower_http::compression::CompressionLayer::new())
        .merge(crate::ui::router())
        .with_state(state)
}

pub(crate) fn base_url(headers: &HeaderMap, scheme: Scheme) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    format!("{}://{host}", scheme.0)
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    let h = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = h.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("oauth")).then(|| token.trim().to_string())
}

fn invalid_session() -> Response {
    SfError::new("INVALID_SESSION_ID", "Session expired or invalid").into_response()
}

/// The OpenID Connect `/userinfo` response; `/id` adds the identity URL's own fields.
fn user_json(st: &AppState, base: &str, user_id: &str) -> Value {
    let conn = st.db.lock().unwrap();
    type Row = (String, String, Option<String>, Option<String>, Option<String>, Option<String>, bool);
    let (username, email, name, first, last, alias, active): Row = conn
        .query_row(
            "SELECT \"Username\", \"Email\", \"Name\", \"FirstName\", \"LastName\", \"Alias\", \"IsActive\" FROM \"User\" WHERE \"Id\" = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get::<_, Option<bool>>(6)?.unwrap_or(true))),
        )
        .unwrap_or_default();
    let org = &st.org_id;
    let v = "{version}";
    json!({
        "sub": format!("{base}/id/{org}/{user_id}"),
        "user_id": user_id,
        "organization_id": org,
        "preferred_username": username,
        "nickname": alias,
        "name": name,
        "email": email,
        "email_verified": true,
        "given_name": first,
        "family_name": last,
        "zoneinfo": "GMT",
        "photos": {
            "picture": format!("{base}/profilephoto/005/F"),
            "thumbnail": format!("{base}/profilephoto/005/T"),
        },
        "profile": format!("{base}/{user_id}"),
        "picture": format!("{base}/profilephoto/005/F"),
        "address": {},
        "is_salesforce_integration_user": false,
        "urls": {
            "enterprise": format!("{base}/services/Soap/c/{v}/{org}"),
            "metadata": format!("{base}/services/Soap/m/{v}/{org}"),
            "partner": format!("{base}/services/Soap/u/{v}/{org}"),
            "rest": format!("{base}/services/data/v{v}/"),
            "sobjects": format!("{base}/services/data/v{v}/sobjects/"),
            "search": format!("{base}/services/data/v{v}/search/"),
            "query": format!("{base}/services/data/v{v}/query/"),
            "recent": format!("{base}/services/data/v{v}/recent/"),
            "tooling_soap": format!("{base}/services/Soap/T/{v}/{org}"),
            "tooling_rest": format!("{base}/services/data/v{v}/tooling/"),
            "profile": format!("{base}/{user_id}"),
            "custom_domain": base,
        },
        "active": active,
        "user_type": "STANDARD",
        "language": "en_US",
        "locale": "en_US",
        "utcOffset": 0,
    })
}

async fn userinfo(State(st): State<AppState>, Extension(scheme): Extension<Scheme>, headers: HeaderMap) -> Response {
    let Some(user) = bearer(&headers).and_then(|t| st.user_for_token(&t)) else { return invalid_session() };
    Json(user_json(&st, &base_url(&headers, scheme), &user)).into_response()
}

async fn identity(
    State(st): State<AppState>,
    Extension(scheme): Extension<Scheme>,
    Path((_org, user)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if bearer(&headers).and_then(|t| st.user_for_token(&t)).is_none() {
        return invalid_session();
    }
    let mut v = user_json(&st, &base_url(&headers, scheme), &user);
    v["id"] = v["sub"].clone();
    v["asserted_user"] = json!(true);
    v["username"] = v["preferred_username"].clone();
    v["nick_name"] = v["nickname"].clone();
    v["display_name"] = v["name"].clone();
    v["first_name"] = v["given_name"].clone();
    v["last_name"] = v["family_name"].clone();
    v["timezone"] = v["zoneinfo"].clone();
    v["mobile_phone"] = Value::Null;
    v["mobile_phone_verified"] = json!(false);
    v["is_lightning_login_user"] = json!(false);
    v["status"] = json!({ "created_date": null, "body": null });
    if let Some(o) = v.as_object_mut() {
        for k in [
            "sub",
            "preferred_username",
            "nickname",
            "name",
            "given_name",
            "family_name",
            "zoneinfo",
            "profile",
            "picture",
            "address",
            "is_salesforce_integration_user",
        ] {
            o.remove(k);
        }
    }
    Json(v).into_response()
}

async fn revoke(State(st): State<AppState>, RawQuery(q): RawQuery, body: Bytes) -> StatusCode {
    let mut params = rest::parse_query(&q.unwrap_or_default());
    params.extend(rest::parse_query(&String::from_utf8_lossy(&body)));
    if let Some(t) = params.get("token") {
        st.sessions.lock().unwrap().remove(t);
        st.revoked.lock().unwrap().insert(t.clone());
    }
    StatusCode::OK
}

/// The text of the first element named `tag` (in any namespace, with any prefix).
fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let node = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == tag)?;
    Some(node.text().unwrap_or_default().to_string())
}

/// Whether the SOAP body calls `op` (e.g. `<n1:login>` or `<logout/>`).
fn soap_call(body: &str, op: &str) -> bool {
    roxmltree::Document::parse(body)
        .map(|doc| doc.descendants().any(|n| n.is_element() && n.tag_name().name() == op))
        .unwrap_or(false)
}

fn soap_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/xml;charset=UTF-8")],
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns="urn:partner.soap.sforce.com" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Body>{body}</soapenv:Body></soapenv:Envelope>"#
        ),
    )
        .into_response()
}

/// The Partner API `login` call (retiring in Summer '27, but still widely used) and `logout`.
async fn soap(
    State(st): State<AppState>,
    Extension(scheme): Extension<Scheme>,
    Path(params): Path<Vec<(String, String)>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let version = params.first().map(|(_, v)| v.clone()).unwrap_or_else(|| "62.0".into());
    let base = base_url(&headers, scheme);
    if soap_call(&body, "logout") {
        if let Some(t) = xml_tag(&body, "sessionId") {
            st.revoked.lock().unwrap().insert(t);
        }
        return soap_response(StatusCode::OK, "<logoutResponse/>".into());
    }
    if !soap_call(&body, "login") {
        let (status, out) = crate::soap::call(&st, &base, &body);
        return soap_response(status, out);
    }
    let username = xml_tag(&body, "username").unwrap_or_default();
    let Some(user) = st.user_by_username(&username) else {
        let message = "Invalid username, password, security token; or user locked out.";
        return soap_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                r#"<soapenv:Fault><faultcode>sf:INVALID_LOGIN</faultcode><faultstring>INVALID_LOGIN: {message}</faultstring><detail><sf:LoginFault xmlns:sf="urn:fault.partner.soap.sforce.com" xsi:type="sf:LoginFault"><sf:exceptionCode>INVALID_LOGIN</sf:exceptionCode><sf:exceptionMessage>{message}</sf:exceptionMessage></sf:LoginFault></detail></soapenv:Fault>"#
            ),
        );
    };
    let token = st.new_session(&user);
    let org = &st.org_id;
    soap_response(
        StatusCode::OK,
        format!(
            "<loginResponse><result><metadataServerUrl>{base}/services/Soap/m/{version}/{org}</metadataServerUrl><passwordExpired>false</passwordExpired><sandbox>true</sandbox><serverUrl>{base}/services/Soap/u/{version}/{org}</serverUrl><sessionId>{token}</sessionId><userId>{user}</userId><userInfo><organizationId>{org}</organizationId><organizationName>localforce</organizationName><userId>{user}</userId><userName>{username}</userName><userTimeZone>GMT</userTimeZone></userInfo></result></loginResponse>"
        ),
    )
}

async fn metadata_soap(State(st): State<AppState>, body: String) -> Response {
    let (status, out) = crate::soap::metadata_call(&st, &body);
    soap_response(status, out)
}

async fn apex_rest(State(st): State<AppState>, headers: HeaderMap, Path(rest): Path<String>) -> Response {
    if bearer(&headers).and_then(|t| st.user_for_token(&t)).is_none() {
        return invalid_session();
    }
    SfError::new("NOT_FOUND", format!("Could not find a match for URL /{rest}")).into_response()
}

async fn data_root(
    state: State<AppState>,
    Extension(scheme): Extension<Scheme>,
    Path(version): Path<String>,
    method: Method,
    mut headers: HeaderMap,
    query: RawQuery,
    body: Bytes,
) -> Response {
    // Some resources (the identity URL, tab icons) have absolute URLs.
    if let Ok(base) = HeaderValue::from_str(&base_url(&headers, scheme)) {
        headers.insert("x-localforce-base-url", base);
    }
    handle(state, version, String::new(), method, headers, query, body)
}

async fn data(
    state: State<AppState>,
    Extension(scheme): Extension<Scheme>,
    Path((version, rest)): Path<(String, String)>,
    method: Method,
    mut headers: HeaderMap,
    query: RawQuery,
    body: Bytes,
) -> Response {
    if let Ok(base) = HeaderValue::from_str(&base_url(&headers, scheme)) {
        headers.insert("x-localforce-base-url", base);
    }
    handle(state, version, rest, method, headers, query, body)
}

fn handle(
    State(st): State<AppState>,
    version: String,
    rest_path: String,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    let Some(v) = version.strip_prefix('v').filter(|v| rest::supported_version(v)) else {
        return SfError::not_found().into_response();
    };
    let Some(user) = bearer(&headers).and_then(|t| st.user_for_token(&t)) else { return invalid_session() };
    let used = st.count_call();
    let ctx = st.ctx(&user, v);

    let body = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) => Some(v),
            Err(e) => {
                return SfError::new("JSON_PARSER_ERROR", format!("Unexpected character: {e}")).into_response();
            }
        }
    };
    // Sforce-Query-Options: batchSize=200 (between 200 and 2000).
    let batch_size = headers
        .get("sforce-query-options")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.split(',').find_map(|kv| kv.trim().strip_prefix("batchSize=").and_then(|n| n.parse::<usize>().ok()))
        })
        .map(|n| n.clamp(200, 2000))
        .unwrap_or(2000);

    let conn = st.db.lock().unwrap();
    let resp = (|| -> rusqlite::Result<rest::Resp> {
        conn.execute_batch("SAVEPOINT lf_request")?;
        let resp = rest::dispatch(
            &conn,
            &ctx,
            Req {
                method,
                segs: rest_path.split('/').map(String::from).collect(),
                query: rest::parse_query(&query.unwrap_or_default()),
                body,
                batch_size,
                st: &st,
                cat: st.cat(),
                headers: headers.clone(),
            },
        );
        if resp.commits() {
            conn.execute_batch("RELEASE lf_request")?;
        } else {
            conn.execute_batch("ROLLBACK TO lf_request; RELEASE lf_request")?;
        }
        Ok(resp)
    })();
    drop(conn);

    let mut out = match resp {
        Ok(r) => {
            let mut out = match r.body {
                Some(b) => (r.status, Json(b)).into_response(),
                None => r.status.into_response(),
            };
            for (k, v) in r.headers {
                if let Ok(v) = HeaderValue::from_str(&v) {
                    out.headers_mut().insert(k, v);
                }
            }
            out
        }
        Err(e) => SfError::from(e).into_response(),
    };
    out.headers_mut()
        .insert("sforce-limit-info", HeaderValue::from_str(&format!("api-usage={used}/{API_LIMIT}")).unwrap());
    out
}
