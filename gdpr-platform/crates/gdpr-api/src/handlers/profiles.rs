use axum::{extract::{State, Path}, Json};
use crate::state::AppState;
use crate::error::ApiResult;

pub async fn list_profiles(
    State(_state): State<AppState>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "profiles": ["standard", "medical", "financial", "legal", "full"]
    })))
}

pub async fn get_profile(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"name": name})))
}
