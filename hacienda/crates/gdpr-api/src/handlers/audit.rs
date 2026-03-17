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

#[derive(Debug, Deserialize)]
pub struct ReviewQueueQuery {
    pub threshold: Option<u32>,
    pub limit:     Option<u32>,
}

#[derive(Serialize)]
pub struct ReviewEntry {
    pub doc_id:       String,
    pub entity_count: usize,
    pub created_at:   i64,
}

#[derive(Serialize)]
pub struct ReviewQueueResp {
    pub documents: Vec<ReviewEntry>,
}

pub async fn get_review_queue(
    State(state): State<AppState>,
    Query(params): Query<ReviewQueueQuery>,
) -> ApiResult<Json<ReviewQueueResp>> {
    let threshold = params.threshold.unwrap_or(20) as i64;
    let limit     = params.limit.unwrap_or(50).min(500) as i64;

    let conn = state.db.get().await
        .map_err(|e| ApiError::Internal(format!("db pool: {e}")))?;

    let rows = conn.interact(move |c| {
        let mut stmt = c.prepare(
            "SELECT e.document_id, COUNT(e.id) AS entity_count, d.created_at
             FROM doc_entity_map e
             JOIN documents d ON d.id = e.document_id
             GROUP BY e.document_id
             HAVING COUNT(e.id) >= ?1
             ORDER BY entity_count DESC
             LIMIT ?2"
        )?;
        let entries: Vec<ReviewEntry> = stmt.query_map(
            rusqlite::params![threshold, limit],
            |row| Ok(ReviewEntry {
                doc_id:       row.get::<_, String>(0)?,
                entity_count: row.get::<_, i64>(1)? as usize,
                created_at:   row.get::<_, i64>(2)?,
            }),
        )?
        .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(entries)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("interact: {e}")))?
    .map_err(|e| ApiError::Internal(format!("sqlite: {e}")))?;

    Ok(Json(ReviewQueueResp { documents: rows }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use dashmap::DashMap;
    use crate::state::AppState;

    async fn test_state() -> AppState {
        let cfg  = deadpool_sqlite::Config::new(":memory:");
        let db   = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        {
            let conn = db.get().await.unwrap();
            conn.interact(|c| c.execute_batch("
                CREATE TABLE IF NOT EXISTS documents (id TEXT PRIMARY KEY, anon_text TEXT NOT NULL, pii_count INTEGER NOT NULL, ner_degraded INTEGER NOT NULL, created_at INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS doc_entity_map (id INTEGER PRIMARY KEY AUTOINCREMENT, document_id TEXT NOT NULL, entity_type TEXT NOT NULL, pseudonym TEXT NOT NULL, detection_layer TEXT NOT NULL, confidence REAL, ner_degraded INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')));
            ")).await.unwrap().unwrap();
        }
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(":memory:").unwrap();
        let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(1, engine).unwrap());
        let dummy_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://127.0.0.1:19999"));
        let meter = Arc::new(gdpr_billing::Meter::new(dummy_ch));
        AppState {
            db, keys: Arc::new(DashMap::new()), http: reqwest::Client::new(),
            upstream_url: String::new(), session_cache: Arc::new(DashMap::new()),
            engine_pool, clickhouse: None, vec_store: None, http_client: reqwest::Client::new(),
            key_cache: Arc::new(DashMap::new()), tensorzero_base_url: String::new(),
            tensorzero_key: String::new(), jwt_secret: String::new(),
            meter, snapshot_cache: Arc::new(DashMap::new()),
        }
    }

    async fn insert_doc_with_entities(state: &AppState, doc_id: &str, entity_count: usize, created_at: i64) {
        let conn = state.db.get().await.unwrap();
        let did  = doc_id.to_string();
        let ts   = created_at;
        conn.interact(move |c| {
            c.execute("INSERT INTO documents (id,anon_text,pii_count,ner_degraded,created_at) VALUES (?1,'',?2,0,?3)",
                rusqlite::params![did, entity_count as i64, ts])?;
            for i in 0..entity_count {
                c.execute("INSERT INTO doc_entity_map (document_id,entity_type,pseudonym,detection_layer,confidence,ner_degraded) VALUES (?1,'PERSON',?2,'L1',0.9,0)",
                    rusqlite::params![did, format!("P{i}")])?;
            }
            Ok::<_,rusqlite::Error>(())
        }).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_review_queue_orders_by_entity_count() {
        let state = test_state().await;
        insert_doc_with_entities(&state, "low",  5,  1710000001).await;
        insert_doc_with_entities(&state, "high", 30, 1710000002).await;
        insert_doc_with_entities(&state, "mid",  15, 1710000003).await;

        let Json(resp) = get_review_queue(
            axum::extract::State(state),
            Query(ReviewQueueQuery { threshold: Some(5), limit: Some(50) }),
        ).await.unwrap();

        assert_eq!(resp.documents.len(), 3);
        assert_eq!(resp.documents[0].doc_id, "high");
        assert_eq!(resp.documents[0].entity_count, 30);
        assert_eq!(resp.documents[1].doc_id, "mid");
        assert_eq!(resp.documents[2].doc_id, "low");
    }

    #[tokio::test]
    async fn test_review_queue_threshold_filters() {
        let state = test_state().await;
        insert_doc_with_entities(&state, "below", 5, 1710000001).await;
        insert_doc_with_entities(&state, "above", 25, 1710000002).await;

        let Json(resp) = get_review_queue(
            axum::extract::State(state),
            Query(ReviewQueueQuery { threshold: Some(20), limit: Some(50) }),
        ).await.unwrap();

        assert_eq!(resp.documents.len(), 1);
        assert_eq!(resp.documents[0].doc_id, "above");
    }

    #[tokio::test]
    async fn test_review_queue_default_threshold_is_20() {
        let state = test_state().await;
        insert_doc_with_entities(&state, "d1", 19, 1710000001).await;
        insert_doc_with_entities(&state, "d2", 21, 1710000002).await;

        let Json(resp) = get_review_queue(
            axum::extract::State(state),
            Query(ReviewQueueQuery { threshold: None, limit: None }),
        ).await.unwrap();

        assert_eq!(resp.documents.len(), 1);
        assert_eq!(resp.documents[0].doc_id, "d2");
    }
}
