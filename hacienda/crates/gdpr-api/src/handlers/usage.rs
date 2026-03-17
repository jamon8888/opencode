use axum::{extract::State, Json};
use crate::state::AppState;
use crate::error::ApiResult;

pub async fn get_usage(State(_state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"tokens_in": 0, "tokens_out": 0, "requests": 0})))
}
