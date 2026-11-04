use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authoritative Redis state is unavailable")]
    RedisUnavailable(#[source] redis::RedisError),
    #[error("authoritative Redis operation timed out")]
    RedisTimeout,
    #[error("stored configuration is invalid: {0}")]
    InvalidStoredConfig(String),
    #[error("admission result is indeterminate: {0}")]
    IndeterminateAdmission(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::RedisUnavailable(_)
            | Self::RedisTimeout
            | Self::InvalidStoredConfig(_)
            | Self::IndeterminateAdmission(_) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
            }
        };

        let body = ErrorBody {
            error,
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

impl From<redis::RedisError> for ApiError {
    fn from(value: redis::RedisError) -> Self {
        Self::RedisUnavailable(value)
    }
}
