use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::error::ApiResult;

#[derive(Deserialize)]
pub struct AnonymizeReq {
    pub text:    String,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct AnonymizeResp {
    pub text:         String,
    pub pii_count:    usize,
    pub ner_degraded: bool,
}

#[derive(Deserialize)]
pub struct DeanonymizeReq {
    pub text:       String,
    pub session_id: Option<String>,
}

#[derive(Serialize)]
pub struct DeanonymizeResp {
    pub text: String,
}

pub async fn post_anonymize(
    State(_state): State<AppState>,
    Json(req): Json<AnonymizeReq>,
) -> ApiResult<Json<AnonymizeResp>> {
    Ok(Json(AnonymizeResp {
        text: req.text,
        pii_count: 0,
        ner_degraded: false,
    }))
}

pub async fn post_deanonymize(
    State(_state): State<AppState>,
    Json(req): Json<DeanonymizeReq>,
) -> ApiResult<Json<DeanonymizeResp>> {
    Ok(Json(DeanonymizeResp { text: req.text }))
}
