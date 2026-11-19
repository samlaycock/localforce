//! The request log: the most recent API calls, with their bodies, for the
//! web UI's "API log" view.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde::Serialize;

use crate::state::AppState;

const KEEP: usize = 500;
/// Bodies bigger than this are truncated in the log (the request itself isn't).
const MAX_BODY: usize = 256 * 1024;

#[derive(Clone, Serialize)]
pub struct Entry {
    pub id: u64,
    pub time: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: f64,
    /// Made by the web UI itself.
    pub ui: bool,
    pub request_headers: Vec<(String, String)>,
    pub request_body: String,
    pub response_body: String,
    /// The first error code in an error response, for the list view.
    pub error_code: Option<String>,
}

#[derive(Default)]
pub struct Inspector {
    entries: Mutex<VecDeque<Entry>>,
    seq: AtomicU64,
}

impl Inspector {
    pub fn list(&self, after: u64) -> Vec<Entry> {
        self.entries.lock().unwrap().iter().filter(|e| e.id > after).cloned().collect()
    }
    pub fn get(&self, id: u64) -> Option<Entry> {
        self.entries.lock().unwrap().iter().find(|e| e.id == id).cloned()
    }
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
    fn push(&self, mut e: Entry) {
        e.id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut entries = self.entries.lock().unwrap();
        if entries.len() == KEEP {
            entries.pop_front();
        }
        entries.push_back(e);
    }
}

fn text(b: &Bytes) -> String {
    let s = String::from_utf8_lossy(&b[..b.len().min(MAX_BODY)]).into_owned();
    if b.len() > MAX_BODY { format!("{s}\n… ({} bytes)", b.len()) } else { s }
}

/// Redact credentials before they're shown.
fn redact(name: &str, value: &str) -> String {
    match name {
        "authorization" => match value.split_once(' ') {
            Some((scheme, token)) if token.len() > 12 => format!("{scheme} {}…", &token[..12]),
            _ => value.to_string(),
        },
        _ => value.to_string(),
    }
}

pub async fn record(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path_and_query().map(|p| p.to_string()).unwrap_or_default();
    let ui = req.headers().contains_key("x-localforce-ui");
    let request_headers = req
        .headers()
        .iter()
        .filter(|(n, _)| !matches!(n.as_str(), "cookie" | "x-localforce-ui"))
        .map(|(n, v)| (n.to_string(), redact(n.as_str(), v.to_str().unwrap_or(""))))
        .collect();
    let (parts, body) = req.into_parts();
    let req_bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
    let mut request_body = text(&req_bytes);
    if path.starts_with("/services/oauth2/token") || path.starts_with("/services/Soap/u") {
        request_body = redact_secrets(&request_body);
    }
    let resp = next.run(Request::from_parts(parts, Body::from(req_bytes))).await;

    let (parts, body) = resp.into_parts();
    let resp_bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
    let response_body = text(&resp_bytes);
    let error_code = (parts.status.as_u16() >= 400)
        .then(|| {
            serde_json::from_slice::<serde_json::Value>(&resp_bytes).ok().and_then(|v| {
                v.get(0)
                    .and_then(|e| e.get("errorCode"))
                    .or_else(|| v.get("error"))
                    .and_then(|c| c.as_str())
                    .map(String::from)
            })
        })
        .flatten();
    st.inspector.push(Entry {
        id: 0,
        time: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        method,
        path,
        status: parts.status.as_u16(),
        duration_ms: (start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0,
        ui,
        request_headers,
        request_body,
        response_body,
        error_code,
    });
    Response::from_parts(parts, Body::from(resp_bytes))
}

/// Hide passwords and client secrets in token and SOAP login requests.
fn redact_secrets(body: &str) -> String {
    let mut out = body.to_string();
    for key in ["password=", "client_secret="] {
        while let Some(i) = out.find(key) {
            let start = i + key.len();
            let end = out[start..].find('&').map(|e| start + e).unwrap_or(out.len());
            if out[start..end] == *"••••" {
                break;
            }
            out.replace_range(start..end, "••••");
        }
    }
    if let (Some(s), Some(e)) = (out.find("<password>"), out.find("</password>")) {
        out.replace_range(s + 10..e, "••••");
    }
    out
}
