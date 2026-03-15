//! HTTP anonymization proxy — `POST /openai/v1/chat/completions`.
//!
//! Pipeline:
//! 1. Collect all text slots from `messages` into a flat `Vec<String>`.
//! 2. `spawn_blocking` → `EnginePool::anonymize_batch` (L1 regex + L2 ONNX).
//! 3. Build a per-session token→original map from the anonymization results.
//! 4. Write cleaned strings back into `req.messages`, preserving structure.
//! 5. Forward to TensorZero.
//! 6. If SSE (`text/event-stream`): stream chunks through `rehydrate_from_cache`,
//!    replacing pseudo-tokens in real time without buffering the full body.
//!    If non-SSE: buffer the body and rehydrate in one pass.
//!
//! Note: This proxy is transparent — OpenCode configures it as the `baseURL` for
//! the `hacienda` provider in `opencode.json`. All conversation traffic flows through it.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::OnceLock;

use gdpr_core::pii::pool::EnginePool;

// ── Token regex ───────────────────────────────────────────────────────────────

static TOKEN_RE: OnceLock<regex::Regex> = OnceLock::new();

fn token_re() -> &'static regex::Regex {
    TOKEN_RE.get_or_init(|| {
        // Matches pseudo-tokens like PERSON_7, IBAN_3, EMAIL_12, etc.
        regex::Regex::new(r"[A-Z][A-Z0-9_]+_\d+").unwrap()
    })
}

// ── Session cache types ───────────────────────────────────────────────────────

pub struct SessionCache {
    /// Maps pseudo-token (e.g. "PERSON_7") → original value (e.g. "Jean Dupont").
    pub token_map:  DashMap<String, String>,
    pub created_at: Instant,
}

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
    pub engine_pool:   Arc<EnginePool>,
    pub http_client:   reqwest::Client,
    pub upstream_url:  String,   // e.g. "http://localhost:3000/openai/v1"
    pub upstream_key:  String,
    /// Per-session token→original maps, keyed by session UUID.
    /// Entries are GC'd after 30 minutes of inactivity (see main.rs).
    pub session_cache: Arc<DashMap<String, SessionCache>>,
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

// ── Rehydration helpers ────────────────────────────────────────────────────────

/// Replace pseudo-tokens in `text` using a shared `Arc<DashMap>` token map.
/// Used in the SSE streaming path where we pass an Arc clone per-chunk.
fn rehydrate_from_cache(
    text: &str,
    token_map: &Option<Arc<DashMap<String, String>>>,
) -> String {
    let Some(map) = token_map else {
        return text.to_string();
    };
    token_re()
        .replace_all(text, |caps: &regex::Captures| {
            map.get(&caps[0])
                .map(|v| v.value().clone())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

/// Replace pseudo-tokens in `text` using an owned `DashMap` token map.
/// Used in the non-SSE buffered path.
fn rehydrate_from_cache_owned(
    text: &str,
    token_map: &Option<DashMap<String, String>>,
) -> String {
    let Some(map) = token_map else {
        return text.to_string();
    };
    token_re()
        .replace_all(text, |caps: &regex::Captures| {
            map.get(&caps[0])
                .map(|v| v.value().clone())
                .unwrap_or_else(|| caps[0].to_string())
        })
        .into_owned()
}

// ── Axum handler ─────────────────────────────────────────────────────────────

pub async fn chat_completions(
    State(state): State<Arc<ProxyState>>,
    _headers:     HeaderMap,
    Json(mut req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    // 1. Collect text slots
    let (raw_texts, slots) = collect_text_slots(&req.messages);

    // Session ID for this request — used to key the token map
    let session_id = uuid::Uuid::new_v4().to_string();

    // 2. spawn_blocking batch anonymize — CPU-bound (regex + ONNX)
    if !raw_texts.is_empty() {
        let pool   = Arc::clone(&state.engine_pool);
        let cloned = raw_texts.clone();

        // Use anonymize_batch (returns AnonymizeResult with entities) so we can
        // extract per-entity token→original mappings for session rehydration.
        let batch_result: Result<Vec<gdpr_core::pii::AnonymizeResult>, _> =
            tokio::task::spawn_blocking(move || {
                let mut _ner_degraded = false;
                pool.anonymize_batch(
                    &cloned.iter().map(String::as_str).collect::<Vec<_>>(),
                    &mut _ner_degraded,
                )
            })
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "spawn_blocking panic — blocking content");
                Err(anyhow::anyhow!("spawn_blocking panic: {e}"))
            });

        match batch_result {
            Ok(results) => {
                // 3. Build session token map: pseudo-token → original value.
                //    AnonymizeResult.mappings is populated by Replacer::pseudonymize and
                //    contains exactly the token→original pairs we need (e.g. "PERSON_7" → "Jean Dupont").
                let token_map: DashMap<String, String> = DashMap::new();
                for anon_result in results.iter() {
                    for (token, original_value) in &anon_result.mappings {
                        token_map.insert(token.clone(), original_value.clone());
                    }
                }

                state.session_cache.insert(session_id.clone(), SessionCache {
                    token_map,
                    created_at: Instant::now(),
                });

                // 4. Write cleaned strings back
                let cleaned: Vec<String> = results.into_iter().map(|r| r.text).collect();
                write_text_slots(&mut req.messages, &cleaned, &slots);
            }
            Err(e) => {
                tracing::error!(error = %e, "anonymize_batch failed — blocking request");
                let fallback = vec![
                    "[PII_DETECTION_ERROR: content blocked]".to_string();
                    raw_texts.len()
                ];
                write_text_slots(&mut req.messages, &fallback, &slots);
            }
        }
    }

    // 5. Forward to TensorZero
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
                // ── SSE streaming path ─────────────────────────────────────
                // Retrieve the Arc'd token map for this session so each chunk
                // closure gets an independent Arc (no borrow across await).
                let token_map: Option<Arc<DashMap<String, String>>> = state
                    .session_cache
                    .get(&session_id)
                    .map(|entry| {
                        // Collect into a new DashMap then wrap in Arc so the
                        // closure owns it independently of the DashMap entry ref.
                        let m: DashMap<String, String> = entry
                            .token_map
                            .iter()
                            .map(|kv| (kv.key().clone(), kv.value().clone()))
                            .collect();
                        Arc::new(m)
                    });
                // Arc keeps the map alive for the stream closure; remove the
                // cache entry immediately so raw PII originals are not retained.
                state.session_cache.remove(&session_id);

                let byte_stream = resp.bytes_stream();
                let mapped = byte_stream.map(move |chunk_result| {
                    let bytes = chunk_result.map_err(|e| e.to_string())?;
                    let text  = String::from_utf8_lossy(&bytes).into_owned();
                    let rehydrated = rehydrate_from_cache(&text, &token_map);
                    Ok::<axum::body::Bytes, String>(axum::body::Bytes::from(rehydrated))
                });

                let body = Body::from_stream(mapped);
                let mut r = Response::new(body);
                *r.status_mut()  = status;
                *r.headers_mut() = headers;
                return r;
            }

            // ── Non-SSE buffered path ──────────────────────────────────────
            match resp.bytes().await {
                Ok(body_bytes) => {
                    let token_map: Option<DashMap<String, String>> = state
                        .session_cache
                        .get(&session_id)
                        .map(|entry| {
                            entry
                                .token_map
                                .iter()
                                .map(|kv| (kv.key().clone(), kv.value().clone()))
                                .collect()
                        });
                    // Remove cache entry immediately; token_map is an owned copy.
                    state.session_cache.remove(&session_id);

                    // For non-2xx upstream errors: return sanitized error, do not rehydrate
                    if !status.is_success() {
                        tracing::warn!(status = %status, "upstream returned error — returning sanitized error");
                        let mut r = Response::new(Body::from(r#"{"error":"upstream error — see server logs"}"#));
                        *r.status_mut() = status;
                        *r.headers_mut() = headers;
                        return r;
                    }

                    let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
                    let rehydrated = rehydrate_from_cache_owned(&body_str, &token_map);
                    let mut r = Response::new(Body::from(rehydrated));
                    *r.status_mut()  = status;
                    *r.headers_mut() = headers;
                    r
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to buffer upstream response for rehydration");
                    let mut r = Response::new(Body::from(format!(
                        r#"{{"error":"upstream read failed: {e}"}}"#
                    )));
                    *r.status_mut() = StatusCode::BAD_GATEWAY;
                    r
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
