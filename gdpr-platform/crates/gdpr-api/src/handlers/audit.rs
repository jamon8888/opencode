use axum::{extract::State, Json};
use crate::state::AppState;
use crate::error::ApiResult;

pub async fn get_audit(State(_state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"events": [], "total": 0})))
}

pub async fn get_review_queue(
    State(_state): State<AppState>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"items": [], "total": 0})))
}
