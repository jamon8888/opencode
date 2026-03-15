use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;
use axum::Json;
use crate::state::ProblemDetail;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Unauthorized,
    RateLimited,
    Validation(String),
    Internal(anyhow::Error),
    Upstream(String),
    UsageCapExceeded,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, slug, detail) = match &self {
            ApiError::NotFound(d)      => (StatusCode::NOT_FOUND,            "not-found",    d.clone()),
            ApiError::Unauthorized     => (StatusCode::UNAUTHORIZED,         "unauthorized", "Invalid or missing API key".to_string()),
            ApiError::RateLimited      => (StatusCode::TOO_MANY_REQUESTS,    "rate-limited", "Rate limit exceeded".to_string()),
            ApiError::Validation(d)    => (StatusCode::UNPROCESSABLE_ENTITY, "validation",   d.clone()),
            ApiError::Internal(e)      => (StatusCode::INTERNAL_SERVER_ERROR,"internal",     e.to_string()),
            ApiError::Upstream(d)      => (StatusCode::BAD_GATEWAY,          "upstream",     d.clone()),
            ApiError::UsageCapExceeded => (StatusCode::PAYMENT_REQUIRED,     "cap-exceeded", "Monthly usage cap exceeded".to_string()),
        };
        let body = ProblemDetail {
            r#type: format!("https://gdpr-platform.eu/errors/{slug}"),
            title:  slug.replace('-', " "),
            status: status.as_u16(),
            detail,
        };
        (status, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}
