use axum::{extract::{Extension, State}, Json};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthContext};

#[derive(Deserialize)]
pub struct SearchReq {
    pub query:   String,
    pub limit:   Option<usize>,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResp {
    pub results: Vec<serde_json::Value>,
}

pub async fn post_search(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(req):       Json<SearchReq>,
) -> ApiResult<Json<SearchResp>> {
    let limit     = req.limit.unwrap_or(10).min(100) as u64;
    let tenant_id = auth.tenant_id.clone();

    // Qdrant path — if available
    if let Some(ref q) = state.qdrant {
        let hits = q
            .search_tenant(&req.query, &tenant_id, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let results = hits
            .into_iter()
            .map(|h| serde_json::json!({
                "doc_id":     h.doc_id,
                "chunk":      h.chunk_text.unwrap_or_default(),
                "score":      h.score,
                "chunk_idx":  h.chunk_idx,
            }))
            .collect();

        return Ok(Json(SearchResp { results }));
    }

    // LIKE fallback — scoped to tenant_id
    let query_pat = format!("%{}%", req.query.replace('%', "\\%").replace('_', "\\_"));
    let conn = state.db.get().await?;
    let results: Vec<serde_json::Value> = conn
        .interact(move |c| {
            let mut stmt = c.prepare(
                "SELECT doc_id, chunk_text, chunk_idx \
                 FROM doc_chunks \
                 WHERE tenant_id = ?1 AND chunk_text LIKE ?2 ESCAPE '\\' \
                 ORDER BY chunk_idx \
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![tenant_id, query_pat, limit as i64],
                |r| Ok(serde_json::json!({
                    "doc_id":    r.get::<_, String>(0)?,
                    "chunk":     r.get::<_, String>(1)?,
                    "score":     1.0f64,
                    "chunk_idx": r.get::<_, i64>(2)?,
                })),
            )?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
        .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    Ok(Json(SearchResp { results }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Extension, State};
    use std::sync::Arc;
    use dashmap::DashMap;

    static VAULT_KEY_SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn ensure_vault_key() {
        VAULT_KEY_SET.get_or_init(|| {
            unsafe { std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-vault-key-32bytespadded!!") };
        });
    }

    async fn make_state_no_qdrant() -> AppState {
        ensure_vault_key();
        let uri = format!("file:gdpr_search_test_{}?mode=memory&cache=shared", uuid::Uuid::new_v4().to_string().replace('-', ""));
        let cfg  = deadpool_sqlite::Config::new(&uri);
        let pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        let conn = pool.get().await.unwrap();
        conn.interact(|c| {
            c.execute_batch("
                CREATE TABLE IF NOT EXISTS doc_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    doc_id TEXT NOT NULL, chunk_idx INTEGER NOT NULL,
                    chunk_text TEXT NOT NULL, byte_offset INTEGER NOT NULL DEFAULT 0,
                    tenant_id TEXT NOT NULL DEFAULT ''
                );
            ")
        }).await.unwrap().unwrap();

        let vault = std::env::temp_dir()
            .join(format!("gdpr-search-test-{}.db", uuid::Uuid::new_v4()));
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(vault.to_str().unwrap()).unwrap();
        let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(1, engine).unwrap());
        let billing_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://localhost:8123"));

        AppState {
            db:                  pool,
            keys:                Arc::new(DashMap::new()),
            http:                reqwest::Client::new(),
            upstream_url:        "".to_string(),
            session_cache:       Arc::new(DashMap::new()),
            engine_pool,
            clickhouse:          None,
            qdrant:              None,
            http_client:         reqwest::Client::new(),
            key_cache:           Arc::new(DashMap::new()),
            tensorzero_base_url: "".to_string(),
            tensorzero_key:      "".to_string(),
            jwt_secret:          "".to_string(),
            meter:               Arc::new(gdpr_billing::Meter::new(billing_ch)),
            snapshot_cache:      Arc::new(DashMap::new()),
        }
    }

    fn auth(tenant: &str) -> crate::state::AuthContext {
        crate::state::AuthContext {
            tenant_id:  tenant.to_string(),
            api_key_id: "k1".to_string(),
            scopes:     vec!["*".to_string()],
            plan:       gdpr_billing::Plan::Starter,
        }
    }

    #[tokio::test]
    async fn test_search_falls_back_to_like_when_no_qdrant() {
        let state = make_state_no_qdrant().await;
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO doc_chunks (doc_id, chunk_idx, chunk_text, byte_offset, tenant_id) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params!["doc1", 0, "hello world test data", 0, "tenant_a"],
            )
        }).await.unwrap().unwrap();

        let Json(resp) = post_search(
            State(state.clone()),
            Extension(auth("tenant_a")),
            Json(SearchReq { query: "hello".to_string(), limit: Some(10), profile: None }),
        ).await.expect("ok");

        assert!(!resp.results.is_empty());
        assert_eq!(resp.results[0]["doc_id"], "doc1");

        // tenant_b sees nothing
        let Json(resp_b) = post_search(
            State(state),
            Extension(auth("tenant_b")),
            Json(SearchReq { query: "hello".to_string(), limit: None, profile: None }),
        ).await.expect("ok");
        assert!(resp_b.results.is_empty());
    }

    #[tokio::test]
    async fn test_search_uses_qdrant_when_available() {
        let qdrant_server = wiremock::MockServer::start().await;
        let emb_server    = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("POST")).and(wiremock::matchers::path("/embeddings"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"embedding": vec![0.1f32; 384]}]
            })))
            .mount(&emb_server).await;

        wiremock::Mock::given(wiremock::matchers::method("POST")).and(wiremock::matchers::path("/collections/gdpr_docs/points/search"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "score": 0.9,
                    "payload": {"doc_id": "qdoc1", "tenant_id": "t1", "chunk_idx": 0, "chunk_text": "qdrant result"}
                }]
            })))
            .mount(&qdrant_server).await;

        let mut state = make_state_no_qdrant().await;
        state.qdrant = {
            let _guard = ENV_MUTEX.lock().unwrap();
            unsafe { std::env::set_var("QDRANT_URL", qdrant_server.uri()) };
            unsafe { std::env::set_var("EMBEDDING_URL", format!("{}/", emb_server.uri())) };
            let q = gdpr_core::clients::qdrant::QdrantStore::from_env();
            // Clean up after ourselves
            unsafe { std::env::remove_var("QDRANT_URL") };
            unsafe { std::env::remove_var("EMBEDDING_URL") };
            q
        };

        let Json(resp) = post_search(
            State(state),
            Extension(auth("t1")),
            Json(SearchReq { query: "find qdrant".to_string(), limit: Some(5), profile: None }),
        ).await.expect("ok");

        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0]["doc_id"], "qdoc1");

        let reqs = qdrant_server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        let must = &body["filter"]["must"][0];
        assert_eq!(must["key"], "tenant_id");
        assert_eq!(must["match"]["value"], "t1");
    }
}
