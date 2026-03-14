//! HTTP anonymization proxy — `POST /openai/v1/chat/completions`.
//!
//! Pipeline:
//! 1. Collect all text slots from `messages` into a flat `Vec<String>`.
//! 2. `spawn_blocking` → `EnginePool::anonymize_batch` (L1 regex + L2 ONNX).
//! 3. Write cleaned strings back into `req.messages`, preserving structure.
//! 4. Forward to TensorZero, stream response verbatim (SSE-safe).
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

use crate::pii::EnginePool;

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
    pub engine_pool:  Arc<EnginePool>,
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

    // 2. spawn_blocking batch anonymize — CPU-bound (regex + ONNX)
    if !raw_texts.is_empty() {
        let pool   = Arc::clone(&state.engine_pool);
        let cloned = raw_texts.clone();

        let cleaned: Vec<String> = tokio::task::spawn_blocking(move || {
            pool.anonymize_batch(&cloned.iter().map(String::as_str).collect::<Vec<_>>())
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "spawn_blocking panic — blocking content");
            vec!["[PII_DETECTION_ERROR: content blocked]".to_string(); raw_texts.len()]
        });

        // 3. Write cleaned strings back
        write_text_slots(&mut req.messages, &cleaned, &slots);
    }

    // 4. Forward to TensorZero, stream response verbatim (SSE ✓)
    match state
        .http_client
        .post(format!("{}/chat/completions", state.upstream_url))
        .header("Authorization", format!("Bearer {}", state.upstream_key))
        .header("Content-Type", "application/json")
        .json(&req)
        .send()
        .await
    {
        Ok(resp) => {
            let status  = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            let mut headers = HeaderMap::new();
            for (k, v) in resp.headers() {
                if let (Ok(n), Ok(val)) = (
                    HeaderName::from_bytes(k.as_str().as_bytes()),
                    HeaderValue::from_bytes(v.as_bytes()),
                ) {
                    headers.insert(n, val);
                }
            }
            let mut r = Response::new(Body::from_stream(resp.bytes_stream()));
            *r.status_mut()  = status;
            *r.headers_mut() = headers;
            r
        }
        Err(e) => {
            let mut r = Response::new(Body::from(format!(r#"{{"error":"{e}"}}"#)));
            *r.status_mut() = StatusCode::BAD_GATEWAY;
            r
        }
    }
}
