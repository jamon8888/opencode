use axum::response::{IntoResponse, Response};
use axum::http::{StatusCode, HeaderValue, header};
use axum::Json;
use serde::{Deserialize, Serialize};

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProblemDetail {
    pub r#type:  String,
    pub title:   String,
    pub status:  u16,
    pub detail:  String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    // Kept from prior version
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("validation error: {0}")]
    Validation(String),
    #[error("internal error: {0}")]
    Internal(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("usage cap exceeded")]
    UsageCapExceeded,

    // New variants
    #[error("request body too large")]
    BodyTooLarge,
    #[error("api key has been revoked")]
    KeyRevoked,
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("pii detection failed")]
    PiiDetectionFailed,
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    #[error("rate limit exceeded, retry after {retry_after_secs}s")]
    RateLimit { retry_after_secs: u64 },
    #[error("database error: {0}")]
    Database(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
}

// Keep backward compat: Internal(anyhow::Error) was used in handlers via From<anyhow::Error>
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<deadpool_sqlite::PoolError> for ApiError {
    fn from(e: deadpool_sqlite::PoolError) -> Self {
        ApiError::Database(e.to_string())
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::Database(e.to_string())
    }
}

impl From<validator::ValidationErrors> for ApiError {
    fn from(e: validator::ValidationErrors) -> Self {
        ApiError::Validation(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, slug, detail) = match &self {
            ApiError::Validation(d)          => (StatusCode::UNPROCESSABLE_ENTITY,    "validation",           d.clone()),
            ApiError::BodyTooLarge           => (StatusCode::PAYLOAD_TOO_LARGE,       "body-too-large",       "Request body exceeds limit".to_string()),
            ApiError::Unauthorized           => (StatusCode::UNAUTHORIZED,            "unauthorized",         "Invalid or missing API key".to_string()),
            ApiError::KeyRevoked             => (StatusCode::UNAUTHORIZED,            "key-revoked",          "API key has been revoked".to_string()),
            ApiError::Forbidden(d)           => (StatusCode::FORBIDDEN,               "forbidden",            d.clone()),
            ApiError::NotFound(d)            => (StatusCode::NOT_FOUND,               "not-found",            d.clone()),
            ApiError::Conflict(d)            => (StatusCode::CONFLICT,                "conflict",             d.clone()),
            ApiError::PiiDetectionFailed     => (StatusCode::UNPROCESSABLE_ENTITY,    "pii-detection-failed", "PII detection failed".to_string()),
            ApiError::UnknownProfile(d)      => (StatusCode::BAD_REQUEST,             "unknown-profile",      d.clone()),
            ApiError::RateLimited            => (StatusCode::TOO_MANY_REQUESTS,       "rate-limited",         "Rate limit exceeded".to_string()),
            ApiError::RateLimit { .. }       => (StatusCode::TOO_MANY_REQUESTS,       "rate-limit",           "Rate limit exceeded".to_string()),
            // 5xx: log internal detail server-side, return generic message to client (GDPR/security)
            ApiError::Database(d)            => { tracing::error!(internal = %d, "database error"); (StatusCode::INTERNAL_SERVER_ERROR,   "database",             "A database error occurred".to_string()) }
            ApiError::Upstream(d)            => { tracing::error!(internal = %d, "upstream error"); (StatusCode::BAD_GATEWAY,             "upstream",             "An upstream service error occurred".to_string()) }
            ApiError::Internal(d)            => { tracing::error!(internal = %d, "internal error"); (StatusCode::INTERNAL_SERVER_ERROR,   "internal",             "An internal error occurred".to_string()) }
            ApiError::ServiceUnavailable(d)  => { tracing::error!(internal = %d, "service unavailable"); (StatusCode::SERVICE_UNAVAILABLE, "service-unavailable",  "Service temporarily unavailable".to_string()) }
            ApiError::UsageCapExceeded       => (StatusCode::PAYMENT_REQUIRED,        "cap-exceeded",         "Monthly usage cap exceeded".to_string()),
        };

        let body = ProblemDetail {
            r#type:  format!("about:{slug}"),
            title:   slug.replace('-', " ").split_whitespace()
                         .map(|w| {
                             let mut c = w.chars();
                             match c.next() {
                                 None    => String::new(),
                                 Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                             }
                         })
                         .collect::<Vec<_>>()
                         .join(" "),
            status:  status.as_u16(),
            detail,
        };

        let mut resp = (status, Json(body)).into_response();

        if let ApiError::RateLimit { retry_after_secs } = &self {
            if let Ok(val) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, val);
            }
        }

        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    fn status(e: ApiError) -> u16 {
        e.into_response().status().as_u16()
    }

    #[test]
    fn test_validation_422()           { assert_eq!(status(ApiError::Validation("x".into())), 422); }
    #[test]
    fn test_body_too_large_413()       { assert_eq!(status(ApiError::BodyTooLarge), 413); }
    #[test]
    fn test_unauthorized_401()         { assert_eq!(status(ApiError::Unauthorized), 401); }
    #[test]
    fn test_key_revoked_401()          { assert_eq!(status(ApiError::KeyRevoked), 401); }
    #[test]
    fn test_forbidden_403()            { assert_eq!(status(ApiError::Forbidden("x".into())), 403); }
    #[test]
    fn test_not_found_404()            { assert_eq!(status(ApiError::NotFound("x".into())), 404); }
    #[test]
    fn test_conflict_409()             { assert_eq!(status(ApiError::Conflict("x".into())), 409); }
    #[test]
    fn test_pii_detection_failed_422() { assert_eq!(status(ApiError::PiiDetectionFailed), 422); }
    #[test]
    fn test_unknown_profile_400()      { assert_eq!(status(ApiError::UnknownProfile("x".into())), 400); }
    #[test]
    fn test_rate_limited_429()         { assert_eq!(status(ApiError::RateLimited), 429); }
    #[test]
    fn test_rate_limit_429()           { assert_eq!(status(ApiError::RateLimit { retry_after_secs: 5 }), 429); }
    #[test]
    fn test_database_500()             { assert_eq!(status(ApiError::Database("x".into())), 500); }
    #[test]
    fn test_upstream_502()             { assert_eq!(status(ApiError::Upstream("x".into())), 502); }
    #[test]
    fn test_internal_500()             { assert_eq!(status(ApiError::Internal("x".into())), 500); }
    #[test]
    fn test_service_unavailable_503()  { assert_eq!(status(ApiError::ServiceUnavailable("x".into())), 503); }
    #[test]
    fn test_usage_cap_exceeded_402()   { assert_eq!(status(ApiError::UsageCapExceeded), 402); }

    #[test]
    fn test_rate_limit_retry_after_header() {
        let resp = ApiError::RateLimit { retry_after_secs: 30 }.into_response();
        assert_eq!(resp.status().as_u16(), 429);
        assert_eq!(
            resp.headers().get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("30")
        );
    }
}
