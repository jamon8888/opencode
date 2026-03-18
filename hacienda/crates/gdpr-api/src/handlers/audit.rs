use axum::extract::{Extension, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::state::{AppState, AuthContext};
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit:  Option<u32>,
    pub action: Option<String>,
    pub doc_id: Option<String>,
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
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(params):   Query<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(ch) = &state.clickhouse else {
        return Ok(Json(serde_json::json!({
            "events": [],
            "total":  0,
            "note":   "ClickHouse not configured (set CLICKHOUSE_URL)"
        })));
    };

    let limit = params.limit.unwrap_or(100).min(1000);

    // Allowlist action values
    const VALID_ACTIONS: &[&str] = &["query", "ingest", "deanonymize", "delete"];
    let action_filter = if let Some(a) = params.action.as_deref() {
        if !VALID_ACTIONS.contains(&a) {
            return Err(ApiError::Validation(format!(
                "invalid action '{}'; allowed: query, ingest, deanonymize, delete", a
            )));
        }
        format!(" AND action = '{a}'")
    } else {
        String::new()
    };

    // tenant_id from JWT (safe to interpolate — validated by middleware)
    let safe_tenant = auth.tenant_id.replace('\'', "''");
    let tenant_filter = format!(" AND tenant_id = '{safe_tenant}'");

    // doc_id must parse as UUID before interpolation
    let doc_filter = if let Some(ref id) = params.doc_id {
        uuid::Uuid::parse_str(id)
            .map_err(|_| ApiError::Validation("invalid doc_id: must be a UUID".to_string()))?;
        format!(" AND document_id = '{id}'")
    } else {
        String::new()
    };

    let query = format!(
        "SELECT document_id, action, pii_count_before, pii_count_after, \
         ner_degraded, processing_time_ms, legal_basis, user_id, model_version, \
         ai_act_risk_level, decision_explanation, ts_unix \
         FROM gdpr.gdpr_audit \
         WHERE 1=1{tenant_filter}{action_filter}{doc_filter} \
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
        let _ = resp.text().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_query_accepts_doc_id() {
        let q: AuditQuery = serde_json::from_str(r#"{"doc_id":"550e8400-e29b-41d4-a716-446655440000","limit":5}"#).unwrap();
        assert_eq!(q.doc_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(q.limit, Some(5));
    }

    #[test]
    fn test_audit_rejects_non_uuid_doc_id() {
        let id = "'; DROP TABLE audit_log; --";
        let valid = uuid::Uuid::parse_str(id).is_ok();
        assert!(!valid);
    }

    #[test]
    fn test_audit_query_without_doc_id() {
        let q: AuditQuery = serde_json::from_str(r#"{"limit":20}"#).unwrap();
        assert!(q.doc_id.is_none());
    }
}
