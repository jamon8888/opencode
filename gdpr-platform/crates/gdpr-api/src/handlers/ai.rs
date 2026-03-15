use axum::{extract::State, http::StatusCode, Json};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(Deserialize, Serialize)]
pub struct ChatMessage {
    pub role:    String,
    pub content: String,
}

#[derive(Deserialize, Serialize)]
pub struct ChatReq {
    pub model:    Option<String>,
    pub messages: Vec<ChatMessage>,
    pub stream:   Option<bool>,
}

#[derive(Deserialize)]
pub struct FeedbackReq {
    pub inference_id:     String,
    pub compliance_score: Option<f32>,
    pub pii_leak:         Option<bool>,
    pub response_quality: Option<f32>,
}

pub async fn post_chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatReq>,
) -> ApiResult<axum::response::Response> {
    let resp = state
        .http
        .post(format!("{}/chat/completions", state.upstream_url))
        .json(&req)
        .send()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = resp
        .bytes()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    Ok((status, body).into_response())
}

pub async fn post_feedback(
    State(state): State<AppState>,
    Json(req): Json<FeedbackReq>,
) -> ApiResult<StatusCode> {
    let (metric_name, value) = match (&req.compliance_score, &req.pii_leak, &req.response_quality) {
        (Some(s), _, _) => ("compliance_score",  serde_json::json!(s)),
        (_, Some(b), _) => ("pii_leak_detected", serde_json::json!(b)),
        (_, _, Some(q)) => ("response_quality",  serde_json::json!(q)),
        _ => return Err(ApiError::Validation("specify at least one metric".into())),
    };

    let tz_base = state.upstream_url.trim_end_matches("/openai/v1");
    state
        .http
        .post(format!("{tz_base}/feedback"))
        .json(&serde_json::json!({
            "inference_id": req.inference_id,
            "metric_name":  metric_name,
            "value":        value,
            "dryrun":       false,
        }))
        .send()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
