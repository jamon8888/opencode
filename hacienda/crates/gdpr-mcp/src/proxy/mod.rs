//! HTTP anonymization proxy — `POST /openai/v1/chat/completions`.
//!
//! Pipeline (T5 thin-client):
//! 1. Collect all text slots from `messages` into a flat `Vec<String>`.
//! 2. Join texts with separator and call `api_client.anonymize`.
//! 3. Split anonymized text back into slots and write into `req.messages`.
//! 4. Forward to TensorZero.
//! 5. Buffer the full response body (SSE or non-SSE).
//! 6. Call `api_client.deanonymize` once to rehydrate pseudo-tokens.
//!    Returns non-streaming even for SSE upstreams (T5 trade-off).
//!
//! Note: This proxy is transparent — OpenCode configures it as the `baseURL` for
//! the `hacienda` provider in `opencode.json`. All conversation traffic flows through it.

use std::sync::Arc;
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::api_client::{ApiClient, AnonymizeRequest, DeanonymizeRequest};

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub part_type: String,
    pub text:      Option<String>,
    #[serde(flatten)]
    pub extra:     Value,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role:    String,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extra:   Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatCompletionRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(flatten)]
    pub extra: Value,
}

// ── Shared state for axum handler ─────────────────────────────────────────────

pub struct ProxyState {
    pub api_client:   Arc<ApiClient>,
    pub http_client:  reqwest::Client,
    pub upstream_url: String,   // e.g. "http://localhost:3000/openai/v1"
    pub upstream_key: String,
}

// ── Helper functions (pub for tests) ──────────────────────────────────────────

/// Collect all text content from messages into a flat Vec.
///
/// Returns `(texts, slots)` where each slot is `(msg_idx, part_idx?)`.
pub fn collect_text_slots(
    messages: &[ChatMessage],
) -> (Vec<String>, Vec<(usize, Option<usize>)>) {
    let mut texts = Vec::new();
    let mut slots = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        match &msg.content {
            MessageContent::Text(t) => {
                texts.push(t.clone());
                slots.push((mi, None));
            }
            MessageContent::Parts(parts) => {
                for (pi, part) in parts.iter().enumerate() {
                    if part.part_type == "text" {
                        if let Some(t) = &part.text {
                            texts.push(t.clone());
                            slots.push((mi, Some(pi)));
                        }
                    }
                }
            }
        }
    }
    (texts, slots)
}

/// Write cleaned texts back into messages at the positions described by `slots`.
pub fn write_text_slots(
    messages:  &mut Vec<ChatMessage>,
    cleaned:   &[String],
    slots:     &[(usize, Option<usize>)],
) {
    for (clean, (mi, pi)) in cleaned.iter().zip(slots.iter()) {
        match pi {
            None => {
                messages[*mi].content = MessageContent::Text(clean.clone());
            }
            Some(p) => {
                if let MessageContent::Parts(ref mut parts) = messages[*mi].content {
                    if let Some(t) = &mut parts[*p].text {
                        *t = clean.clone();
                    }
                }
            }
        }
    }
}

// ── Axum handler ─────────────────────────────────────────────────────────────

pub async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    _headers:     HeaderMap,
    Json(mut req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // 1. Collect text slots
    let (raw_texts, slots) = collect_text_slots(&req.messages);

    // 2. Anonymize via gdpr-api
    let session_id: Option<String>;
    if !raw_texts.is_empty() {
        let joined = raw_texts.join("\n---\n");
        match state.api_client.anonymize(AnonymizeRequest {
            text: joined,
            legal_basis: "legitimate_interest".to_string(),
            ..Default::default()
        }).await {
            Ok(anon_resp) => {
                session_id = Some(anon_resp.session_id);
                // Split the anonymized text back into slots
                let cleaned: Vec<String> = anon_resp.anonymized_text
                    .splitn(raw_texts.len(), "\n---\n")
                    .map(|s| s.to_string())
                    .collect();
                write_text_slots(&mut req.messages, &cleaned, &slots);
            }
            Err(e) => {
                tracing::error!(error = %e, "gdpr-api anonymize failed — blocking request");
                session_id = None;
                let fallback = vec!["[PII_DETECTION_ERROR: content blocked]".to_string(); raw_texts.len()];
                write_text_slots(&mut req.messages, &fallback, &slots);
            }
        }
    } else {
        session_id = None;
    }

    // 3. Forward to TensorZero
    let upstream_resp = state
        .http_client
        .post(format!("{}/chat/completions", state.upstream_url))
        .header("Authorization", format!("Bearer {}", state.upstream_key))
        .header("Content-Type", "application/json")
        .json(&req)
        .send()
        .await;

    match upstream_resp {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);

            // Copy headers, dropping Content-Length (body size may change after rehydration).
            let mut headers = HeaderMap::new();
            let is_sse = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(|ct| ct.contains("text/event-stream"))
                .unwrap_or(false);

            for (k, v) in resp.headers() {
                if k.as_str().eq_ignore_ascii_case("content-length") {
                    continue;
                }
                if let (Ok(n), Ok(val)) = (
                    HeaderName::from_bytes(k.as_str().as_bytes()),
                    HeaderValue::from_bytes(v.as_bytes()),
                ) {
                    headers.insert(n, val);
                }
            }

            if is_sse {
                // Buffer the full SSE response, then deanonymize once (T5 trade-off)
                match resp.bytes().await {
                    Ok(body_bytes) => {
                        let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
                        let rehydrated = if let Some(ref sid) = session_id {
                            match state.api_client.deanonymize(DeanonymizeRequest {
                                text: body_str.clone(),
                                session_id: Some(sid.clone()),
                                ..Default::default()
                            }).await {
                                Ok(r) => r.text,
                                Err(e) => {
                                    tracing::warn!(error = %e, "deanonymize failed — returning anonymized text");
                                    body_str
                                }
                            }
                        } else {
                            body_str
                        };
                        let mut r = Response::new(Body::from(rehydrated));
                        *r.status_mut() = status;
                        r
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to buffer SSE response");
                        let mut r = Response::new(Body::from(r#"{"error":"upstream read failed"}"#));
                        *r.status_mut() = StatusCode::BAD_GATEWAY;
                        r
                    }
                }
            } else {
                // Non-SSE buffered path
                match resp.bytes().await {
                    Ok(body_bytes) => {
                        if !status.is_success() {
                            let mut r = Response::new(Body::from(r#"{"error":"upstream error — see server logs"}"#));
                            *r.status_mut() = status;
                            *r.headers_mut() = headers;
                            return r;
                        }
                        let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
                        let rehydrated = if let Some(ref sid) = session_id {
                            match state.api_client.deanonymize(DeanonymizeRequest {
                                text: body_str.clone(),
                                session_id: Some(sid.clone()),
                                ..Default::default()
                            }).await {
                                Ok(r) => r.text,
                                Err(e) => {
                                    tracing::warn!(error = %e, "deanonymize failed — returning anonymized text");
                                    body_str
                                }
                            }
                        } else {
                            body_str
                        };
                        let mut r = Response::new(Body::from(rehydrated));
                        *r.status_mut() = status;
                        *r.headers_mut() = headers;
                        r
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to read upstream response body");
                        let mut r = Response::new(Body::from(r#"{"error":"upstream read failed"}"#));
                        *r.status_mut() = StatusCode::BAD_GATEWAY;
                        r
                    }
                }
            }
        }
        Err(e) => {
            let mut r = Response::new(Body::from(format!(r#"{{"error":"{e}"}}"#)));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            r
        }
    }
}
