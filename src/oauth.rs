//! OAuth 2.0: the token endpoint's grants (password, client credentials,
//! refresh token, authorization code with PKCE, and the JWT bearer flow) and
//! the authorization endpoint, which shows a user picker instead of a login
//! form. Any client id and secret are accepted; there are no connected apps.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use rand::Rng;
use rand::distributions::Alphanumeric;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::rest;
use crate::routes::{Scheme, base_url};
use crate::state::AppState;

/// Authorization codes last 15 minutes, like Salesforce's.
const CODE_TTL: Duration = Duration::from_secs(15 * 60);

/// An authorization code waiting to be exchanged for a token.
struct Code {
    user: String,
    redirect_uri: String,
    /// The PKCE challenge and its method (`S256` or `plain`).
    challenge: Option<(String, String)>,
    issued: Instant,
}

/// Outstanding authorization codes and the refresh tokens handed out.
#[derive(Default)]
pub struct Grants {
    codes: std::sync::Mutex<HashMap<String, Code>>,
    /// refresh token -> user id
    refresh: std::sync::Mutex<HashMap<String, String>>,
}

fn random(n: usize) -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(n).map(char::from).collect()
}

fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (status, Json(json!({ "error": error, "error_description": description }))).into_response()
}

fn invalid_grant(description: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", description)
}

/// A token response; with `refresh`, a refresh token for the same user too.
fn token_response(st: &AppState, base: &str, user_id: &str, refresh: bool) -> Value {
    let token = st.new_session(user_id);
    let mut v = json!({
        "access_token": token,
        "instance_url": base,
        "id": format!("{base}/id/{}/{user_id}", st.org_id),
        "token_type": "Bearer",
        "issued_at": chrono::Utc::now().timestamp_millis().to_string(),
        "signature": URL_SAFE.encode(Sha256::digest(token.as_bytes())),
        "scope": "api refresh_token full",
    });
    if refresh {
        let r = format!("5Aep861{}", random(80));
        st.grants.refresh.lock().unwrap().insert(r.clone(), user_id.to_string());
        v["refresh_token"] = json!(r);
    }
    v
}

/// `POST /services/oauth2/token`.
pub async fn token(
    State(st): State<AppState>,
    Extension(scheme): Extension<Scheme>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = rest::parse_query(&String::from_utf8_lossy(&body));
    let base = base_url(&headers, scheme);
    let param = |k: &str| form.get(k).map(String::as_str).unwrap_or_default();
    match form.get("grant_type").map(String::as_str) {
        Some("password") => match st.user_by_username(param("username")) {
            Some(u) => Json(token_response(&st, &base, &u, false)).into_response(),
            None => invalid_grant("authentication failure"),
        },
        Some("client_credentials") => Json(token_response(&st, &base, &st.default_user, false)).into_response(),
        Some("refresh_token") => {
            let known = st.grants.refresh.lock().unwrap().get(param("refresh_token")).cloned();
            match known {
                Some(u) => Json(token_response(&st, &base, &u, false)).into_response(),
                // Like unknown access tokens, unknown refresh tokens (from an
                // earlier run, say) are the default user's unless sessions are strict.
                None if !st.config.strict_sessions && !param("refresh_token").is_empty() => {
                    Json(token_response(&st, &base, &st.default_user, false)).into_response()
                }
                None => invalid_grant("expired access/refresh token"),
            }
        }
        Some("authorization_code") => {
            let code = st.grants.codes.lock().unwrap().remove(param("code"));
            let Some(code) = code.filter(|c| c.issued.elapsed() < CODE_TTL) else {
                return invalid_grant("invalid authorization code");
            };
            if param("redirect_uri") != code.redirect_uri {
                return invalid_grant("redirect_uri must match configuration");
            }
            let verifier = param("code_verifier");
            if code.challenge.is_none() && !verifier.is_empty() {
                return invalid_grant("invalid code verifier");
            }
            if let Some((challenge, method)) = &code.challenge {
                let derived = if method == "plain" {
                    verifier.to_string()
                } else {
                    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
                };
                if verifier.is_empty() || derived != *challenge {
                    return invalid_grant("invalid code verifier");
                }
            }
            Json(token_response(&st, &base, &code.user, true)).into_response()
        }
        Some("urn:ietf:params:oauth:grant-type:jwt-bearer") => jwt_bearer(&st, &base, param("assertion")),
        Some(other) => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            &format!("grant type not supported: {other}"),
        ),
        None => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "grant_type is required"),
    }
}

/// The JWT bearer flow: the assertion's `sub` is the username. Its claims are
/// checked (issuer, audience, expiry) but not its signature: localforce has no
/// connected app certificate to check it against.
fn jwt_bearer(st: &AppState, base: &str, assertion: &str) -> Response {
    let parts: Vec<&str> = assertion.split('.').collect();
    let claims: Option<Value> = (parts.len() == 3)
        .then(|| URL_SAFE_NO_PAD.decode(parts[1].trim_end_matches('=')).ok())
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let Some(claims) = claims else { return invalid_grant("invalid assertion") };
    let text = |k: &str| claims.get(k).and_then(Value::as_str).unwrap_or_default();
    // `prn` is the old name for `sub`.
    let subject = if text("sub").is_empty() { text("prn") } else { text("sub") };
    if text("iss").is_empty() {
        return invalid_grant("invalid client credentials");
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(s)) => vec![s.as_str()],
        _ => vec![],
    };
    if !audiences.iter().any(|a| audience_ok(a, base)) {
        return invalid_grant("audience is invalid");
    }
    // A number, or (as some clients send it) a string of one.
    let exp = match claims.get("exp") {
        Some(Value::String(s)) => s.parse::<i64>().ok(),
        Some(v) => v.as_i64(),
        None => None,
    };
    match exp {
        // Three minutes' allowance for clock skew.
        Some(exp) if exp + 180 > chrono::Utc::now().timestamp() => {}
        _ => return invalid_grant("expired authorization code"),
    }
    match st.user_by_username(subject) {
        Some(u) => Json(token_response(st, base, &u, false)).into_response(),
        None => invalid_grant("user hasn't approved this consumer"),
    }
}

/// Salesforce's login hosts (and My Domains), or localforce itself.
fn audience_ok(aud: &str, base: &str) -> bool {
    let aud = aud.trim_end_matches('/');
    if aud.eq_ignore_ascii_case(base) {
        return true;
    }
    url::Url::parse(aud).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)).is_some_and(|h| {
        h.ends_with(".salesforce.com") || h == "localhost" || h == "127.0.0.1" || h.ends_with(".localhost")
    })
}

/// `GET /services/oauth2/authorize`: pick a user to sign in as (or, with a
/// `login_hint` naming one, sign in straight away), then go back to the
/// client's `redirect_uri` with a code (`response_type=code`) or a token
/// (`response_type=token`, in the fragment).
pub async fn authorize(
    State(st): State<AppState>,
    Extension(scheme): Extension<Scheme>,
    headers: HeaderMap,
    RawQuery(q): RawQuery,
) -> Response {
    let params = rest::parse_query(&q.unwrap_or_default());
    if let Some(r) = check_authorize(&params) {
        return r;
    }
    let hinted = params.get("login_hint").and_then(|u| st.user_by_username(u));
    if let Some(user) = hinted {
        return finish(&st, &base_url(&headers, scheme), &params, &user);
    }
    Html(picker(&st, &params)).into_response()
}

/// `POST /services/oauth2/authorize`: the picker's choice.
pub async fn authorize_choice(
    State(st): State<AppState>,
    Extension(scheme): Extension<Scheme>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = rest::parse_query(&String::from_utf8_lossy(&body));
    if let Some(r) = check_authorize(&params) {
        return r;
    }
    if params.contains_key("lf_deny") {
        return redirect_with(
            &params,
            &[("error", "access_denied"), ("error_description", "end-user denied authorization")],
        );
    }
    let user = params.get("lf_user").cloned().unwrap_or_default();
    let active: bool = {
        let conn = st.db.lock().unwrap();
        conn.query_row("SELECT EXISTS (SELECT 1 FROM \"User\" WHERE \"Id\" = ?1 AND \"IsActive\" = 1)", [&user], |r| {
            r.get(0)
        })
        .unwrap_or(false)
    };
    if !active {
        return (
            StatusCode::BAD_REQUEST,
            Html(page("Unknown user", "<p>That user doesn't exist or isn't active.</p>")),
        )
            .into_response();
    }
    finish(&st, &base_url(&headers, scheme), &params, &user)
}

/// A response refusing the request, if it can't go ahead.
fn check_authorize(params: &HashMap<String, String>) -> Option<Response> {
    let redirect = params.get("redirect_uri").map(String::as_str).unwrap_or_default();
    if url::Url::parse(redirect).is_err() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Html(page(
                    "Missing redirect_uri",
                    "<p>error=redirect_uri_missing: the request needs a valid <code>redirect_uri</code>.</p>",
                )),
            )
                .into_response(),
        );
    }
    if params.get("client_id").is_none_or(|c| c.is_empty()) {
        return Some(redirect_with(
            params,
            &[("error", "invalid_client_id"), ("error_description", "client identifier invalid")],
        ));
    }
    match params.get("response_type").map(String::as_str) {
        Some("code" | "token") => None,
        _ => Some(redirect_with(
            params,
            &[("error", "unsupported_response_type"), ("error_description", "response type not supported")],
        )),
    }
}

/// Back to the client with a code or a token.
fn finish(st: &AppState, base: &str, params: &HashMap<String, String>, user: &str) -> Response {
    if params.get("response_type").map(String::as_str) == Some("token") {
        let t = token_response(st, base, user, false);
        let fields: Vec<(&str, String)> =
            ["access_token", "instance_url", "id", "issued_at", "signature", "scope", "token_type"]
                .into_iter()
                .map(|k| (k, t[k].as_str().unwrap_or_default().to_string()))
                .collect();
        let refs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        return redirect_with(params, &refs);
    }
    let code = format!("aPrx{}", random(84));
    let challenge = params
        .get("code_challenge")
        .filter(|c| !c.is_empty())
        .map(|c| (c.clone(), params.get("code_challenge_method").cloned().unwrap_or_else(|| "S256".into())));
    let mut codes = st.grants.codes.lock().unwrap();
    codes.retain(|_, c| c.issued.elapsed() < CODE_TTL);
    codes.insert(
        code.clone(),
        Code {
            user: user.to_string(),
            redirect_uri: params["redirect_uri"].clone(),
            challenge,
            issued: Instant::now(),
        },
    );
    drop(codes);
    redirect_with(params, &[("code", &code)])
}

/// A redirect to the client's `redirect_uri`, with `state` passed back: in
/// the query for codes and errors, in the fragment for tokens.
fn redirect_with(params: &HashMap<String, String>, values: &[(&str, &str)]) -> Response {
    let mut out = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in values {
        out.append_pair(k, v);
    }
    if let Some(s) = params.get("state") {
        out.append_pair("state", s);
    }
    let encoded = out.finish();
    let target = &params["redirect_uri"];
    let fragment = values.iter().any(|(k, _)| *k == "access_token");
    let sep = if fragment {
        "#"
    } else if target.contains('?') {
        "&"
    } else {
        "?"
    };
    Redirect::to(&format!("{target}{sep}{encoded}")).into_response()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn picker(st: &AppState, params: &HashMap<String, String>) -> String {
    let users: Vec<(String, String, String)> = {
        let conn = st.db.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT \"Id\", IFNULL(\"Name\", \"Username\"), \"Username\" FROM \"User\" WHERE \"IsActive\" = 1 ORDER BY \"CreatedDate\", \"Id\"")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().filter_map(Result::ok).collect()
    };
    let hidden: String = params
        .iter()
        .filter(|(k, _)| !k.starts_with("lf_"))
        .map(|(k, v)| format!(r#"<input type="hidden" name="{}" value="{}">"#, esc(k), esc(v)))
        .collect();
    let client = params.get("client_id").map(String::as_str).unwrap_or_default();
    let target =
        url::Url::parse(&params["redirect_uri"]).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
    let buttons: String = users
        .iter()
        .map(|(id, name, username)| {
            format!(
                r#"<button type="submit" name="lf_user" value="{}"><strong>{}</strong><span>{}</span></button>"#,
                esc(id),
                esc(name),
                esc(username)
            )
        })
        .collect();
    page(
        "Sign in",
        &format!(
            r#"<p>An app (<code>{}</code>) wants to use the local org. Who do you want to sign in as?</p>
<form method="post" action="/services/oauth2/authorize">{hidden}<div class="users">{buttons}</div><button type="submit" name="lf_deny" value="1" class="deny">Deny access</button></form>
<p class="note">You'll go back to <strong>{}</strong>. localforce doesn't check passwords; pass <code>login_hint=&lt;username&gt;</code> to skip this page.</p>"#,
            esc(client),
            esc(&target)
        ),
    )
}

fn page(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{} · localforce</title><style>
:root {{ color-scheme: light dark; --bg: #f6f7f9; --card: #fff; --ink: #1d2330; --muted: #5b6475; --line: #dde1e8; --accent: #2f6fde; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg: #14171c; --card: #1c2027; --ink: #e6e9ef; --muted: #9aa3b2; --line: #2c323c; --accent: #6b9cf0; }} }}
body {{ margin: 0; background: var(--bg); color: var(--ink); font: 15px/1.5 system-ui, sans-serif; display: grid; place-items: center; min-height: 100vh; }}
main {{ background: var(--card); border: 1px solid var(--line); border-radius: 10px; padding: 24px; width: min(440px, calc(100vw - 32px)); box-sizing: border-box; }}
h1 {{ font-size: 18px; margin: 0 0 8px; }} .note {{ color: var(--muted); font-size: 13px; }}
.users {{ display: grid; gap: 8px; margin: 16px 0; }}
button {{ display: grid; text-align: left; gap: 2px; padding: 10px 12px; border: 1px solid var(--line); border-radius: 8px; background: transparent; color: inherit; font: inherit; cursor: pointer; }}
button:hover, button:focus-visible {{ border-color: var(--accent); outline: none; }} button span {{ color: var(--muted); font-size: 13px; }}
.deny {{ display: inline; border: 0; padding: 0; color: var(--muted); font-size: 13px; text-decoration: underline; }}
code {{ font-size: 13px; }}
</style></head><body><main><h1>localforce · {}</h1>{body}</main></body></html>"#,
        esc(title),
        esc(title)
    )
}
