use axum::{extract::{Query, State}, Json};
use serde::{Deserialize, Serialize};

use crate::state::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit:  Option<u32>,
    pub action: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEvent {
    pub document_id:         String,
    pub action:              String,
    pub pii_count_before:    u32,
    pub pii_count_after:     u32,
    pub ner_degraded:        u8,
    pub processing_time_ms:  u32,
    pub legal_basis:         String,
    pub user_id:             String,
    pub model_version:       String,
    pub ai_act_risk_level:   String,
    pub decision_explanation: String,
    pub ts_unix:             u64,
}

pub async fn get_audit(
    State(state): State<AppState>,
    Query(params): Query<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(ch) = &state.clickhouse else {
        return Ok(Json(serde_json::json!({
            "events": [],
            "total":  0,
            "note":   "ClickHouse not configured (set CLICKHOUSE_URL)"
        })));
    };

    let limit  = params.limit.unwrap_or(100).min(1000);
    // Allowlist: only permit known action values to prevent SQL injection
    const VALID_ACTIONS: &[&str] = &["query", "ingest", "deanonymize", "delete"];
    let filter = if let Some(a) = params.action.as_deref() {
        if !VALID_ACTIONS.contains(&a) {
            return Err(ApiError::Validation(format!(
                "invalid action '{}'; allowed: query, ingest, deanonymize, delete", a
            )));
        }
        format!(" AND action = '{a}'")
    } else {
        String::new()
    };

    let query = format!(
        "SELECT document_id, action, pii_count_before, pii_count_after, \
         ner_degraded, processing_time_ms, legal_basis, user_id, model_version, \
         ai_act_risk_level, decision_explanation, ts_unix \
         FROM gdpr.gdpr_audit \
         WHERE 1=1{filter} \
         ORDER BY ts_unix DESC \
         LIMIT {limit} \
         FORMAT JSONEachRow",
    );

    let url = format!(
        "{}/?query={}",
        ch.base_url().trim_end_matches('/'),
        urlencoding::encode(&query)
    );

    let resp = ch.http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let _ = resp.text().await; // consume body
        tracing::warn!(%status, "ClickHouse audit query failed");
        return Err(ApiError::Upstream(format!("ClickHouse error {status}")));
    }

    let body = resp.text().await.map_err(|e| ApiError::Upstream(e.to_string()))?;
    let events: Vec<AuditEvent> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let total = events.len();
    Ok(Json(serde_json::json!({ "events": events, "total": total })))
}

pub async fn get_review_queue(
    State(_state): State<AppState>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"items": [], "total": 0})))
}
