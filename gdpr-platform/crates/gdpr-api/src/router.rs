use axum::{Router, routing::{get, post, delete}};
use tower_http::{
    cors::CorsLayer,
    compression::CompressionLayer,
    timeout::TimeoutLayer,
    limit::RequestBodyLimitLayer,
    trace::TraceLayer,
};
use std::time::Duration;
use crate::state::AppState;
use crate::handlers;

pub fn build(state: AppState) -> Router {
    Router::new()
        // Health
        .route("/health",  get(handlers::health::get_health))
        .route("/metrics", get(handlers::health::get_metrics))
        // Documents
        .route("/v1/documents",     post(handlers::documents::post_document))
        .route("/v1/documents",     get(handlers::documents::list_documents))
        .route("/v1/documents/:id", get(handlers::documents::get_document))
        .route("/v1/documents/:id", delete(handlers::documents::delete_document))
        // Search
        .route("/v1/search", post(handlers::search::post_search))
        // Anonymize
        .route("/v1/anonymize",   post(handlers::anonymize::post_anonymize))
        .route("/v1/deanonymize", post(handlers::anonymize::post_deanonymize))
        // AI
        .route("/v1/ai/chat/completions", post(handlers::ai::post_chat_completions))
        .route("/v1/ai/feedback",         post(handlers::ai::post_feedback))
        // Audit
        .route("/v1/audit",        get(handlers::audit::get_audit))
        .route("/v1/review-queue", get(handlers::audit::get_review_queue))
        // Profiles
        .route("/v1/profiles",       get(handlers::profiles::list_profiles))
        .route("/v1/profiles/:name", get(handlers::profiles::get_profile))
        // Usage
        .route("/v1/usage", get(handlers::usage::get_usage))
        // Keys
        .route("/v1/keys",     post(handlers::keys::post_key))
        .route("/v1/keys/:id", delete(handlers::keys::delete_key))
        // Session
        .route("/v1/session/:id/table", get(handlers::session::get_session_table))
        .route("/v1/session/:id",       delete(handlers::session::delete_session))
        .with_state(state)
        .layer(CompressionLayer::new())
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::REQUEST_TIMEOUT, Duration::from_secs(30)))
        .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
}
