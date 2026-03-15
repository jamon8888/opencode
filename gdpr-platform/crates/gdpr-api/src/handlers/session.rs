use axum::{extract::{State, Path}, http::StatusCode, Json};
use crate::state::AppState;
use crate::error::{ApiError, ApiResult};

pub async fn get_session_table(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    if let Some(session) = state.session_cache.get(&id) {
        let table: Vec<_> = session
            .token_map
            .iter()
            .map(|entry| serde_json::json!({"token": entry.key(), "original": entry.value()}))
            .collect();
        Ok(Json(serde_json::json!({"session_id": id, "entries": table})))
    } else {
        Err(ApiError::NotFound(format!("session {id} not found")))
    }
}

pub async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.session_cache.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
