use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// One error as Salesforce reports it: an error code such as
/// `REQUIRED_FIELD_MISSING`, a message and the fields involved.
#[derive(Clone, Debug)]
pub struct SfError {
    pub code: &'static str,
    pub message: String,
    pub fields: Vec<String>,
}

impl SfError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        SfError { code, message: message.into(), fields: vec![] }
    }
    pub fn with_fields(mut self, fields: Vec<String>) -> Self {
        self.fields = fields;
        self
    }

    /// The REST shape: `{"message", "errorCode", "fields"}`.
    pub fn rest_json(&self) -> serde_json::Value {
        let mut v = json!({ "message": self.message, "errorCode": self.code });
        if !self.fields.is_empty() {
            v["fields"] = json!(self.fields);
        }
        v
    }

    /// The shape inside DML results (collections, composite): `statusCode` instead of `errorCode`.
    pub fn save_result_json(&self) -> serde_json::Value {
        json!({ "statusCode": self.code, "message": self.message, "fields": self.fields })
    }

    pub fn status(&self) -> StatusCode {
        match self.code {
            "NOT_FOUND" | "ENTITY_IS_DELETED" => StatusCode::NOT_FOUND,
            "INVALID_SESSION_ID" => StatusCode::UNAUTHORIZED,
            "METHOD_NOT_ALLOWED" => StatusCode::METHOD_NOT_ALLOWED,
            "REQUEST_LIMIT_EXCEEDED" => StatusCode::FORBIDDEN,
            "UNKNOWN_EXCEPTION" => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        }
    }

    pub fn not_found() -> Self {
        SfError::new("NOT_FOUND", "The requested resource does not exist")
    }
}

impl From<rusqlite::Error> for SfError {
    fn from(e: rusqlite::Error) -> Self {
        tracing::error!("sqlite: {e}");
        SfError::new("UNKNOWN_EXCEPTION", format!("An unexpected error occurred: {e}"))
    }
}

/// REST errors are always a JSON array, even for a single error.
impl IntoResponse for SfError {
    fn into_response(self) -> Response {
        (self.status(), Json(json!([self.rest_json()]))).into_response()
    }
}

pub type SfResult<T> = Result<T, SfError>;
