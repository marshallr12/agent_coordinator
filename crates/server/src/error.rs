use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// Only sanitized public messages are retained. Underlying errors may contain SQL
/// parameters, and must never be returned to a client or logged here.
pub struct AppError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub details: Value,
}

impl AppError {
    pub fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
            details: json!({}),
        }
    }
    pub fn bad_request(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    pub fn conflict(code: &str, message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    pub fn forbidden(message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, "operation_not_permitted", message)
    }
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "record_not_found",
            "The requested record was not found.",
        )
    }
    pub fn auth_required() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Sign in or configure this workstation's agent credential, then reconnect.",
        )
        .with_details(json!({"help_path":"/api/v1/help/authentication"}))
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    pub(crate) fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The operation could not be completed.",
        )
    }
    pub(crate) fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many sign-in attempts. Wait one minute before trying again.",
        )
    }
}

impl std::fmt::Debug for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppError")
            .field("status", &self.status)
            .field("code", &self.code)
            .finish()
    }
}
impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for AppError {}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        match error {
            sqlx::Error::RowNotFound => Self::not_found(),
            sqlx::Error::Database(ref db) if db.is_unique_violation() => Self::conflict(
                "record_conflict",
                "A record with that identity already exists.",
            ),
            sqlx::Error::Database(ref db)
                if matches!(db.code().as_deref(), Some("5" | "6" | "261" | "517")) =>
            {
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporarily_unavailable",
                    "The service is busy. Retry with the same idempotency key.",
                )
            }
            sqlx::Error::PoolTimedOut => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "The service is busy. Retry with the same idempotency key.",
            ),
            _ => Self::internal(),
        }
    }
}
impl From<serde_json::Error> for AppError {
    fn from(_: serde_json::Error) -> Self {
        Self::bad_request("The request must contain valid JSON.")
    }
}
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let retryable = matches!(
            self.status,
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        );
        let next_actions = if self.status == StatusCode::UNAUTHORIZED {
            json!([{"action":"show_operator_setup_help"}])
        } else {
            json!([])
        };
        let mut result = (
            self.status,
            Json(json!({
                "request_id": uuid::Uuid::new_v4().to_string(),
                "server_time": chrono::Utc::now().to_rfc3339(),
                "error": { "code": self.code, "message": self.message,
                    "details": self.details, "next_actions": next_actions, "retryable": retryable }
            })),
        )
            .into_response();
        if self.status == StatusCode::TOO_MANY_REQUESTS {
            result
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("60"));
        }
        result
    }
}
