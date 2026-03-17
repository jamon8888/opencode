use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::error::ApiResult;

#[derive(Deserialize)]
pub struct SearchReq {
    pub query:   String,
    pub limit:   Option<usize>,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResp {
    pub results: Vec<serde_json::Value>,
}

pub async fn post_search(
    State(_state): State<AppState>,
    Json(_req): Json<SearchReq>,
) -> ApiResult<Json<SearchResp>> {
    Ok(Json(SearchResp { results: vec![] }))
}
