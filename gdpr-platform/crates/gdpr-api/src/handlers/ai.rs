use axum::{body::Body, extract::State, http::StatusCode, response::IntoResponse};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, SessionCache};
use dashmap::DashMap;

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

// ── Token regex (same pattern as gdpr-mcp proxy) ─────────────────────────────

fn token_re() -> &'static regex::Regex {
    static TOKEN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    TOKEN_RE.get_or_init(|| regex::Regex::new(r"[A-Z][A-Z0-9_]+_\d+").unwrap())
}

// ── Rehydration helpers ───────────────────────────────────────────────────────

fn rehydrate_chunk(
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

fn rehydrate_buffered(
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

// ── Handler ───────────────────────────────────────────────────────────────────

pub async fn post_chat_completions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::Json(mut req): axum::Json<ChatReq>,
) -> ApiResult<axum::response::Response> {
    // ── Step 1: Collect message contents for batch anonymization ──────────
    let raw_texts: Vec<String> = req.messages.iter().map(|m| m.content.clone()).collect();

    // ── Step 2: spawn_blocking → anonymize_batch ──────────────────────────
    let pool = Arc::clone(&state.engine_pool);
    let batch_results = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = raw_texts.iter().map(|s| s.as_str()).collect();
        let mut ner_degraded = false;
        pool.anonymize_batch(&refs, &mut ner_degraded)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("spawn_blocking join error: {e}")))?
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Capture PII entity count before batch_results is consumed by later iterators
    let pii_count: u32 = batch_results.iter().map(|r| r.mappings.len() as u32).sum();

    // ── Step 3: Build session token map and store in session_cache ────────
    let token_map: DashMap<String, String> = DashMap::new();
    for result in &batch_results {
        for (token, original) in &result.mappings {
            token_map.insert(token.clone(), original.clone());
        }
    }
    let session_id = uuid::Uuid::new_v4().to_string();
    state.session_cache.insert(session_id.clone(), SessionCache {
        token_map,
        created_at: std::time::Instant::now(),
    });

    // ── Step 4: Write anonymized texts back into messages ─────────────────
    let cleaned: Vec<String> = batch_results.iter().map(|r| r.text.clone()).collect();
    for (msg, clean) in req.messages.iter_mut().zip(cleaned.iter()) {
        msg.content = clean.clone();
    }

    // ── Step 5: Forward anonymized request to TensorZero ─────────────────
    let mut request_builder = state
        .http
        .post(format!("{}/chat/completions", state.upstream_url))
        .json(&req);
    if let Some(rid) = headers.get("x-request-id") {
        request_builder = request_builder.header("x-request-id", rid);
    }
    let resp = request_builder
        .send()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    // Copy upstream headers, drop Content-Length (body may change after rehydration).
    let mut resp_headers = axum::http::HeaderMap::new();
    for (k, v) in resp.headers() {
        if k.as_str().eq_ignore_ascii_case("content-length") {
            continue;
        }
        if let (Ok(n), Ok(val)) = (
            axum::http::HeaderName::from_bytes(k.as_str().as_bytes()),
            axum::http::HeaderValue::from_bytes(v.as_bytes()),
        ) {
            resp_headers.insert(n, val);
        }
    }

    if is_sse {
        // ── 6a: SSE streaming path ─────────────────────────────────────────
        let token_map_arc: Option<Arc<DashMap<String, String>>> = state
            .session_cache
            .get(&session_id)
            .map(|entry| {
                let m: DashMap<String, String> = entry
                    .token_map
                    .iter()
                    .map(|kv| (kv.key().clone(), kv.value().clone()))
                    .collect();
                Arc::new(m)
            });
        state.session_cache.remove(&session_id); // GDPR: don't hold PII longer than needed

        let byte_stream = resp.bytes_stream();
        let mapped = byte_stream.map(move |chunk_result| {
            let bytes = chunk_result.map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let rehydrated = rehydrate_chunk(&text, &token_map_arc);
            Ok::<axum::body::Bytes, String>(axum::body::Bytes::from(rehydrated))
        });

        let body = Body::from_stream(mapped);
        let mut r = axum::response::Response::new(body);
        *r.status_mut() = status;
        *r.headers_mut() = resp_headers;

        // Emit GDPR Art. 30 audit row for SSE path (best-effort)
        if let Some(ch) = &state.clickhouse {
            let row = crate::clients::GdprAuditRow {
                document_id:          session_id.clone(),
                action:               "query".to_string(),
                pii_count_before:     pii_count,
                pii_count_after:      0,
                ner_degraded:         0,
                processing_time_ms:   0,
                legal_basis:          "contract".to_string(),
                user_id:              "anonymous".to_string(),
                model_version:        "gdpr-api".to_string(),
                ai_act_risk_level:    "low".to_string(),
                decision_explanation: String::new(),
            };
            let ch = Arc::clone(ch);
            tokio::spawn(async move { ch.write_audit_row(row).await });
        }

        return Ok(r);
    }

    // ── 6b: Non-SSE buffered path ──────────────────────────────────────────
    let body_bytes = resp
        .bytes()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    // For non-2xx upstream errors: sanitize — don't rehydrate PII back into the error body
    if !status.is_success() {
        tracing::warn!(status = %status, "upstream returned error — returning sanitized error to caller");
        let mut r = axum::response::Response::new(Body::from(
            r#"{"error":"upstream error — see server logs"}"#,
        ));
        *r.status_mut() = status;
        *r.headers_mut() = resp_headers;
        return Ok(r);
    }

    // Only reach here for 2xx responses — safe to rehydrate
    let token_map_opt: Option<DashMap<String, String>> = state
        .session_cache
        .get(&session_id)
        .map(|entry| {
            entry
                .token_map
                .iter()
                .map(|kv| (kv.key().clone(), kv.value().clone()))
                .collect()
        });
    state.session_cache.remove(&session_id); // GDPR: don't hold PII longer than needed

    let text = String::from_utf8_lossy(&body_bytes).into_owned();
    let rehydrated = rehydrate_buffered(&text, &token_map_opt);

    // Extract token counts and insert usage record (best-effort)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&rehydrated) {
        let tokens_in  = json["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
        let tokens_out = json["usage"]["completion_tokens"].as_i64().unwrap_or(0);
        if tokens_in > 0 || tokens_out > 0 {
            let conn = state.db.get().await;
            if let Ok(conn) = conn {
                let month = chrono::Utc::now().format("%Y-%m").to_string();
                let ts    = chrono::Utc::now().timestamp();
                let _ = conn.interact(move |c| {
                    c.execute(
                        "INSERT INTO usage_records (api_key_id, tokens_in, tokens_out, month, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        rusqlite::params!["anonymous", tokens_in, tokens_out, month, ts],
                    )
                }).await;
            }
        }
    }

    // Emit GDPR Art. 30 audit row (best-effort — never blocks response)
    if let Some(ch) = &state.clickhouse {
        let row = crate::clients::GdprAuditRow {
            document_id:          session_id.clone(),
            action:               "query".to_string(),
            pii_count_before:     pii_count,
            pii_count_after:      0,
            ner_degraded:         0,
            processing_time_ms:   0,
            legal_basis:          "contract".to_string(),
            user_id:              "anonymous".to_string(),
            model_version:        "gdpr-api".to_string(),
            ai_act_risk_level:    "low".to_string(),
            decision_explanation: String::new(),
        };
        let ch = Arc::clone(ch);
        tokio::spawn(async move { ch.write_audit_row(row).await });
    }

    let mut r = axum::response::Response::new(Body::from(rehydrated));
    *r.status_mut() = status;
    *r.headers_mut() = resp_headers;
    Ok(r)
}

pub async fn post_feedback(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<FeedbackReq>,
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
