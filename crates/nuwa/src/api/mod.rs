// M2a Task 2 lands the error contract before the routes that consume it.
// Remove this allow once the API handlers are wired.
#![allow(dead_code)]

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub fields: Option<BTreeMap<String, String>>,
}

#[derive(Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Serialize)]
pub struct ErrorDetail {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, String>>,
}

impl From<ApiError> for ErrorBody {
    fn from(e: ApiError) -> Self {
        Self {
            error: ErrorDetail {
                code: e.code,
                message: e.message,
                fields: e.fields,
            },
        }
    }
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), fields: None }
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "invalid_state", message)
    }
    pub fn validation(fields: BTreeMap<String, String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "validation_failed",
            message: "validation failed".into(),
            fields: Some(fields),
        }
    }
    pub fn rate_limited(retry_after_ms: i64) -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!("retry after {retry_after_ms}ms"),
        )
    }
    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "upstream_error", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status;
        (status, Json(ErrorBody::from(self))).into_response()
    }
}

pub fn api_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", "unknown API endpoint")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_serializes_shape() {
        let body = ErrorBody::from(ApiError::not_found("issue not found"));
        let value = serde_json::to_value(body).expect("json");
        assert_eq!(value["error"]["code"], "not_found");
        assert_eq!(value["error"]["message"], "issue not found");
    }
}
