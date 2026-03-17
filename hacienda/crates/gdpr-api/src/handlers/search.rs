use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::error::{ApiError, ApiResult};

#[derive(Deserialize)]
pub struct SearchReq {
    pub query:   String,
    pub limit:   Option<usize>,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub doc_id: String,
    pub chunk:  String,
    pub score:  f64,
}

#[derive(Serialize)]
pub struct SearchResp {
    pub results: Vec<SearchResult>,
}

pub async fn post_search(
    State(state): State<AppState>,
    Json(req): Json<SearchReq>,
) -> ApiResult<Json<SearchResp>> {
    let limit  = req.limit.unwrap_or(10).min(100);
    let query  = req.query.clone();

    // TODO T6: try VecStore semantic search if `state.vec_store` is Some
    // For T5: SQLite LIKE fallback only
    let conn = state.db.get().await
        .map_err(|e| ApiError::Internal(format!("db pool: {e}")))?;

    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let results = conn.interact(move |c| {
        let mut stmt = c.prepare(
            "SELECT doc_id, chunk_text FROM doc_chunks
             WHERE chunk_text LIKE ?1 ESCAPE '\\'
             LIMIT ?2"
        )?;
        let rows: Vec<SearchResult> = stmt.query_map(
            rusqlite::params![pattern, limit as i64],
            |row| Ok(SearchResult {
                doc_id: row.get::<_, String>(0)?,
                chunk:  row.get::<_, String>(1)?,
                score:  1.0, // keyword match = binary score
            }),
        )?
        .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("interact: {e}")))?
    .map_err(|e| ApiError::Internal(format!("sqlite: {e}")))?;

    Ok(Json(SearchResp { results }))
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
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS documents (id TEXT PRIMARY KEY, anon_text TEXT NOT NULL, pii_count INTEGER NOT NULL, ner_degraded INTEGER NOT NULL, created_at INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS doc_chunks (id TEXT PRIMARY KEY, doc_id TEXT NOT NULL, chunk_idx INTEGER NOT NULL, chunk_text TEXT NOT NULL, chunk_offset INTEGER NOT NULL, created_at INTEGER NOT NULL);
                CREATE TABLE IF NOT EXISTS doc_entity_map (id INTEGER PRIMARY KEY AUTOINCREMENT, document_id TEXT NOT NULL, entity_type TEXT NOT NULL, pseudonym TEXT NOT NULL, detection_layer TEXT NOT NULL, confidence REAL, ner_degraded INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')));
            ")).await.unwrap().unwrap();
        }
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(":memory:").unwrap();
        let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(1, engine).unwrap());
        let dummy_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://127.0.0.1:19999"));
        let meter = Arc::new(gdpr_billing::Meter::new(dummy_ch));
        AppState {
            db,
            keys:          Arc::new(DashMap::new()),
            http:          reqwest::Client::new(),
            upstream_url:  String::new(),
            session_cache: Arc::new(DashMap::new()),
            engine_pool,
            clickhouse:    None,
            vec_store:     None,
            http_client:   reqwest::Client::new(),
            key_cache:     Arc::new(DashMap::new()),
            tensorzero_base_url: String::new(),
            tensorzero_key:      String::new(),
            jwt_secret:          String::new(),
            meter,
            snapshot_cache: Arc::new(DashMap::new()),
        }
    }

    #[tokio::test]
    async fn test_post_search_returns_matching_chunks() {
        let state = test_state().await;
        {
            let conn = state.db.get().await.unwrap();
            conn.interact(|c| {
                c.execute("INSERT INTO documents (id,anon_text,pii_count,ner_degraded,created_at) VALUES ('d1','text',0,0,1710000000)", [])?;
                c.execute("INSERT INTO doc_chunks (id,doc_id,chunk_idx,chunk_text,chunk_offset,created_at) VALUES ('c1','d1',0,'GDPR compliance notice about data retention',0,1710000000)", [])?;
                c.execute("INSERT INTO doc_chunks (id,doc_id,chunk_idx,chunk_text,chunk_offset,created_at) VALUES ('c2','d1',1,'Unrelated content about weather',1000,1710000000)", [])?;
                Ok::<_,rusqlite::Error>(())
            }).await.unwrap().unwrap();
        }
        let req = SearchReq { query: "GDPR compliance".to_string(), limit: Some(10), profile: None };
        let Json(resp) = post_search(
            axum::extract::State(state),
            axum::Json(req),
        ).await.unwrap();
        assert_eq!(resp.results.len(), 1, "only chunk containing 'GDPR compliance' should match");
        assert_eq!(resp.results[0].doc_id, "d1");
        assert!(resp.results[0].chunk.contains("GDPR compliance"));
    }

    #[tokio::test]
    async fn test_post_search_empty_when_no_match() {
        let state = test_state().await;
        let req = SearchReq { query: "nonexistent_xyz_123".to_string(), limit: None, profile: None };
        let Json(resp) = post_search(axum::extract::State(state), axum::Json(req)).await.unwrap();
        assert!(resp.results.is_empty());
    }
}
