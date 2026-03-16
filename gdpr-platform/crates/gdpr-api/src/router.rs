use axum::{Router, routing::{get, post, delete}};
use tower::ServiceBuilder;
use tower_http::{
    cors::CorsLayer,
    compression::CompressionLayer,
    sensitive_headers::SetSensitiveHeadersLayer,
    timeout::TimeoutLayer,
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use std::time::Duration;
use crate::state::AppState;
use crate::handlers;
use crate::middleware::{AuthLayer, RateLimitLayer, MeterLayer, SecurityHeadersLayer};

pub fn build(state: AppState) -> Router {
    // Public routes (no auth required)
    let public = Router::new()
        .route("/health", get(handlers::health::get_health))
        .route("/metrics", get(handlers::health::get_metrics))
        .route("/v1/profiles", get(handlers::profiles::list_profiles))
        .route("/v1/profiles/:name", get(handlers::profiles::get_profile));

    // Authenticated routes
    let authed = Router::new()
        // Anonymize
        .route("/v1/anonymize", post(handlers::anonymize::post_anonymize))
        .route("/v1/deanonymize", post(handlers::anonymize::post_deanonymize))
        // Documents
        .route("/v1/documents", post(handlers::documents::post_document))
        .route("/v1/documents", get(handlers::documents::list_documents))
        .route("/v1/documents/:id", get(handlers::documents::get_document))
        .route("/v1/documents/:id", delete(handlers::documents::delete_document))
        // Search
        .route("/v1/search", post(handlers::search::post_search))
        // Usage
        .route("/v1/usage", get(handlers::usage::get_usage))
        // Audit
        .route("/v1/audit", get(handlers::audit::get_audit))
        .route("/v1/review-queue", get(handlers::audit::get_review_queue))
        // Session
        .route("/v1/session/:id/table", get(handlers::session::get_session_table))
        .route("/v1/session/:id", delete(handlers::session::delete_session))
        // Keys
        .route("/v1/keys", post(handlers::keys::post_key))
        .route("/v1/keys/:id", delete(handlers::keys::delete_key))
        .route("/v1/keys/:id/rotate", post(handlers::keys::rotate_key))
        // AI
        .route("/v1/ai/chat/completions", post(handlers::ai::post_chat_completions))
        .route("/v1/ai/feedback", post(handlers::ai::post_feedback))
        // Auth middleware stack (order: outermost layer runs first)
        .layer(MeterLayer::new(state.clone()))
        .layer(RateLimitLayer::new())
        .layer(AuthLayer::new(state.clone()));

    // Wire global middleware
    let (set_req_id, prop_req_id) = crate::middleware::make_request_id();

    Router::new()
        .merge(public)
        .merge(authed)
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024)) // 10MB
        .layer(
            ServiceBuilder::new()
                .layer(set_req_id)
                .layer(prop_req_id)
                .layer(TraceLayer::new_for_http())
                .layer(SetSensitiveHeadersLayer::new([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::COOKIE,
                ]))
                .layer(TimeoutLayer::new(Duration::from_secs(30)))
                .layer(CompressionLayer::new())
                .layer(CorsLayer::permissive())
                .layer(SecurityHeadersLayer),
        )
        .with_state(state)
}
