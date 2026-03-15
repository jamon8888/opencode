use axum::{extract::{State, Path}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::error::ApiResult;

#[derive(Deserialize)]
pub struct IngestReq {
    pub text:        String,
    pub legal_basis: Option<String>,
    pub profile:     Option<String>,
}

#[derive(Serialize)]
pub struct IngestResp {
    pub doc_id:            String,
    pub pii_count:         usize,
    pub ner_degraded:      bool,
    pub ai_act_risk_level: String,
}

#[derive(Serialize)]
pub struct DocList {
    pub items: Vec<serde_json::Value>,
    pub total: usize,
}

pub async fn post_document(
    State(_state): State<AppState>,
    Json(_req): Json<IngestReq>,
) -> ApiResult<(StatusCode, Json<IngestResp>)> {
    let doc_id = uuid::Uuid::new_v4().to_string();
    // Stub: full implementation delegates to gdpr-core engine_pool
    Ok((
        StatusCode::CREATED,
        Json(IngestResp {
            doc_id,
            pii_count: 0,
            ner_degraded: false,
            ai_act_risk_level: "low".to_string(),
        }),
    ))
}

pub async fn list_documents(
    State(_state): State<AppState>,
) -> ApiResult<Json<DocList>> {
    Ok(Json(DocList { items: vec![], total: 0 }))
}

pub async fn get_document(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"id": id, "status": "ok"})))
}

pub async fn delete_document(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> ApiResult<StatusCode> {
    Ok(StatusCode::NO_CONTENT)
}
