use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthContext};

// ── GET /v1/session/:id/table ────────────────────────────────────────────────

pub async fn get_session_table(
    State(state): State<AppState>,
    Extension(auth_ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    auth_ctx.require_scope("read:vault")?;

    if let Some(session) = state.session_cache.get(&id) {
        let table: Vec<_> = session
            .token_map
            .iter()
            .map(|entry| serde_json::json!({"token": entry.key(), "original": entry.value()}))
            .collect();
        Ok(Json(serde_json::json!({"session_id": id, "token_map": table})))
    } else {
        Err(ApiError::NotFound(format!("session {id} not found")))
    }
}

// ── DELETE /v1/session/:id ───────────────────────────────────────────────────

pub async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.session_cache.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}
