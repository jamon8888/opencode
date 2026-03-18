use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use gdpr_core::pii::{
    AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool,
};

use rusqlite::OptionalExtension;

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthContext, SessionCache};

// ── Request / Response ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct IngestReq {
    pub text:        String,
    pub legal_basis: Option<String>,
    pub profile:     Option<String>,
}

#[derive(Serialize)]
pub struct IngestResp {
    pub doc_id:               String,
    pub session_id:           String,
    pub pii_count:            usize,
    pub ner_degraded:         bool,
    pub ai_act_risk_level:    String,
    pub chunk_count:          usize,
    pub decision_explanation: String,
}

// ── POST /v1/documents ────────────────────────────────────────────────────────

pub async fn post_document(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(req):       Json<IngestReq>,
) -> ApiResult<(StatusCode, Json<IngestResp>)> {
    let doc_id     = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let text       = req.text;
    let _legal_basis = req.legal_basis.unwrap_or_else(|| "legitimate_interest".to_string());
    let tenant_id  = auth.tenant_id.clone();

    // Parse profile
    let profile: AnonProfile = match req.profile.as_deref() {
        None => AnonProfile::default(),
        Some(p) => serde_json::from_value(serde_json::Value::String(p.to_string()))
            .map_err(|_| ApiError::UnknownProfile(format!("Unknown profile: {p}")))?,
    };

    // Anonymize (L1 regex, CPU-bound)
    let mut session_ctx  = SessionContext::new(profile);
    let pool_strings: Vec<String> = get_pool(&profile).iter().map(|s| s.to_string()).collect();
    let engine  = TreatmentEngine::new(pool_strings);
    let text2   = text.clone();
    let result  = tokio::task::spawn_blocking(move || {
        anonymize_with_profile(&text2, profile, &mut session_ctx, &engine)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("spawn_blocking join: {e}")))?
    .map_err(|e| ApiError::Internal(format!("anonymize_with_profile: {e}")))?;

    // Populate session_cache (for deanonymize calls)
    {
        let dm = dashmap::DashMap::new();
        for (token, original) in &result.token_map {
            dm.insert(token.clone(), original.clone());
        }
        state.session_cache.insert(session_id.clone(), SessionCache {
            token_map:  dm,
            created_at: std::time::Instant::now(),
        });
    }

    // Chunk the anonymized text (~200 words per chunk)
    let words: Vec<&str> = result.text.split_whitespace().collect();
    let chunk_size = 200usize;
    // byte_pos tracks the offset in the whitespace-normalised representation
    // (split_whitespace → join(" ")). Consumers reading chunks back use the
    // same normalisation, so offsets are self-consistent.
    let mut byte_pos = 0usize;
    let chunk_rows: Vec<(usize, String, usize)> = words
        .chunks(chunk_size)
        .enumerate()
        .map(|(i, w)| {
            let start  = byte_pos;
            let joined = w.join(" ");
            byte_pos  += joined.len() + 1; // +1 for the space between chunks
            (i, joined, start)
        })
        .collect();

    let chunk_count = chunk_rows.len();

    // SQLite writes
    let doc_id2         = doc_id.clone();
    let anonymized_text = result.text.clone();
    let original_text   = text.clone();
    let tenant_id2      = tenant_id.clone();
    let pii_count       = result.pii_count;
    let ner_degraded    = result.ner_degraded;
    let ai_act_risk     = "low".to_string();
    let chunk_rows2     = chunk_rows.clone();
    let token_map2      = result.token_map.clone();

    let conn = state.db.get().await?;
    conn.interact(move |c| {
        let tx = c.transaction()?;
        let now = chrono::Utc::now().timestamp();
        tx.execute(
            "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![doc_id2, original_text, anonymized_text, now, tenant_id2],
        )?;
        for (idx, chunk_text, byte_offset) in &chunk_rows2 {
            tx.execute(
                "INSERT OR IGNORE INTO doc_chunks (doc_id, chunk_idx, chunk_text, byte_offset, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![doc_id2, idx, chunk_text, byte_offset, tenant_id2],
            )?;
        }
        for (token, original) in &token_map2 {
            tx.execute(
                "INSERT OR IGNORE INTO doc_entity_map (document_id, entity_type, original_value, pseudonym, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![doc_id2, "PII", original, token, tenant_id2],
            )?;
        }
        tx.commit()?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    // Fire-and-forget Qdrant upsert
    if let Some(ref q) = state.qdrant {
        let q          = Arc::clone(q);
        let chunks: Vec<(usize, String)> = chunk_rows
            .iter()
            .map(|(idx, text, _offset)| (*idx, text.clone()))
            .collect();
        let doc_id3    = doc_id.clone();
        let tenant_id3 = auth.tenant_id.clone();
        tokio::spawn(async move {
            if let Err(e) = q.upsert_chunks_tenant(&doc_id3, &tenant_id3, &chunks).await {
                tracing::warn!(error = %e, doc_id = %doc_id3, "qdrant upsert failed");
            }
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(IngestResp {
            doc_id,
            session_id,
            pii_count,
            ner_degraded,
            ai_act_risk_level:    ai_act_risk,
            chunk_count,
            decision_explanation: format!(
                "Processed with profile {:?}, {} PII entities replaced",
                profile, pii_count
            ),
        }),
    ))
}

// ── GET /v1/documents ─────────────────────────────────────────────────────────

pub async fn list_documents(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = auth.tenant_id.clone();
    let conn = state.db.get().await?;
    let items: Vec<serde_json::Value> = conn.interact(move |c| {
        let mut stmt = c.prepare(
            "SELECT d.id AS doc_id, d.created_at, COUNT(e.id) AS entity_count
             FROM documents d
             LEFT JOIN doc_entity_map e ON e.document_id = d.id
             WHERE d.tenant_id = ?1
             GROUP BY d.id
             ORDER BY d.created_at DESC",
        )?;
        let rows = stmt.query_map([&tenant_id], |r| {
            Ok(serde_json::json!({
                "doc_id":       r.get::<_, String>(0)?,
                "created_at":   r.get::<_, i64>(1)?,
                "entity_count": r.get::<_, i64>(2)?,
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    let total = items.len();
    Ok(Json(serde_json::json!({ "items": items, "total": total })))
}

// ── GET /v1/documents/:id ────────────────────────────────────────────────────

pub async fn get_document(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id):        Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = auth.tenant_id.clone();
    let conn = state.db.get().await?;
    let row: Option<serde_json::Value> = conn.interact(move |c| -> Result<Option<serde_json::Value>, rusqlite::Error> {
        c.query_row(
            "SELECT id, anonymized_text, created_at FROM documents WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![id, tenant_id],
            |r| Ok(serde_json::json!({
                "id":              r.get::<_, String>(0)?,
                "anonymized_text": r.get::<_, String>(1)?,
                "created_at":      r.get::<_, i64>(2)?,
            })),
        ).optional()
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    row.map(Json).ok_or_else(|| ApiError::NotFound("document not found".to_string()))
}

// ── DELETE /v1/documents/:id ──────────────────────────────────────────────────

pub async fn delete_document(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id):        Path<String>,
) -> ApiResult<StatusCode> {
    let tenant_id = auth.tenant_id.clone();
    let id2       = id.clone();
    let conn = state.db.get().await?;

    let deleted: bool = conn.interact(move |c| {
        let tx = c.transaction()?;

        // Delete children first, then parent — all within the same transaction.
        // The final delete on `documents` tells us whether the doc existed for this tenant.
        tx.execute("DELETE FROM doc_chunks     WHERE doc_id      = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        tx.execute("DELETE FROM doc_entity_map WHERE document_id = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        let rows = tx.execute("DELETE FROM documents      WHERE id          = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        tx.commit()?;

        Ok::<_, rusqlite::Error>(rows > 0)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    if !deleted {
        return Err(ApiError::NotFound(format!("document '{}' not found", id)));
    }

    // Fire-and-forget Qdrant delete
    if let Some(ref q) = state.qdrant {
        let q          = Arc::clone(q);
        let doc_id3    = id.clone();
        let tenant_id3 = auth.tenant_id.clone();
        tokio::spawn(async move {
            if let Err(e) = q.delete_chunks_tenant(&doc_id3, &tenant_id3).await {
                tracing::warn!(error = %e, doc_id = %doc_id3, "qdrant delete failed");
            }
        });
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Extension, State};
    use std::sync::Arc;
    use dashmap::DashMap;
    use deadpool_sqlite::Config;

    static VAULT_KEY_SET: std::sync::OnceLock<()> = std::sync::OnceLock::new();

    fn ensure_vault_key() {
        VAULT_KEY_SET.get_or_init(|| {
            unsafe { std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-vault-key-32bytespadded!!") };
        });
    }

    /// Build a minimal in-memory AppState for document handler tests.
    async fn make_state() -> AppState {
        // Use a unique named shared-cache in-memory database so that all
        // connections in the pool share the same schema and data.
        let db_name = format!("gdpr_test_{}", uuid::Uuid::new_v4().simple());
        let uri = format!("file:{}?mode=memory&cache=shared", db_name);
        let cfg  = Config::new(&uri);
        let pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        let conn = pool.get().await.unwrap();
        conn.interact(|c| {
            c.execute_batch("
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY, original_text TEXT NOT NULL,
                    anonymized_text TEXT NOT NULL, created_at INTEGER NOT NULL,
                    tenant_id TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE IF NOT EXISTS doc_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, doc_id TEXT NOT NULL,
                    chunk_idx INTEGER NOT NULL, chunk_text TEXT NOT NULL,
                    byte_offset INTEGER NOT NULL DEFAULT 0,
                    tenant_id TEXT NOT NULL DEFAULT '',
                    UNIQUE(doc_id, chunk_idx, tenant_id)
                );
                CREATE TABLE IF NOT EXISTS doc_entity_map (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, document_id TEXT NOT NULL,
                    entity_type TEXT NOT NULL, original_value TEXT NOT NULL,
                    pseudonym TEXT NOT NULL, tenant_id TEXT NOT NULL DEFAULT '',
                    UNIQUE(document_id, pseudonym, tenant_id)
                );
            ")
        }).await.unwrap().unwrap();

        let vault = std::env::temp_dir()
            .join(format!("gdpr-t6-test-{}.db", uuid::Uuid::new_v4()));
        ensure_vault_key();
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(
            vault.to_str().unwrap(),
        ).expect("test engine");
        let engine_pool = Arc::new(
            gdpr_core::pii::pool::EnginePool::new(1, engine).expect("test pool"),
        );

        let billing_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://localhost:8123"));
        AppState {
            db:                  pool,
            keys:                Arc::new(DashMap::new()),
            http:                reqwest::Client::new(),
            upstream_url:        "http://localhost:3000".to_string(),
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

    fn auth(tenant: &str) -> AuthContext {
        AuthContext {
            tenant_id:  tenant.to_string(),
            api_key_id: "key1".to_string(),
            scopes:     vec!["*".to_string()],
            plan:       gdpr_billing::Plan::Starter,
        }
    }

    #[tokio::test]
    async fn test_post_document_stores_tenant_id() {
        let state = make_state().await;
        let req = IngestReq {
            text:        "Hello world, contact jean.dupont@example.com".to_string(),
            legal_basis: Some("consent".to_string()),
            profile:     Some("max".to_string()),
        };
        let resp = post_document(
            State(state.clone()),
            Extension(auth("tenant_a")),
            Json(req),
        ).await.expect("post_document failed");

        let (status, Json(body)) = resp;
        assert_eq!(status, StatusCode::CREATED);
        assert!(!body.session_id.is_empty());
        uuid::Uuid::parse_str(&body.session_id).expect("session_id must be valid UUID");

        let doc_id = body.doc_id.clone();
        let conn = state.db.get().await.unwrap();
        let (count, stored_tenant): (i64, String) = conn.interact(move |c| {
            c.query_row(
                "SELECT COUNT(*), tenant_id FROM documents WHERE id = ?1",
                [&doc_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        }).await.unwrap().unwrap();
        assert_eq!(count, 1);
        assert_eq!(stored_tenant, "tenant_a");
    }

    #[tokio::test]
    async fn test_ingest_resp_has_session_id() {
        let state = make_state().await;
        let req = IngestReq {
            text:        "Test text without PII".to_string(),
            legal_basis: None,
            profile:     None,
        };
        let (_, Json(body)) = post_document(
            State(state.clone()),
            Extension(auth("t1")),
            Json(req),
        ).await.expect("ok");
        assert!(!body.session_id.is_empty());
        assert_eq!(body.session_id.len(), 36);
    }

    #[tokio::test]
    async fn test_tenant_isolation_list() {
        let state = make_state().await;
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-a1", "orig", "anon", 1735689600i64, "tenant_a"],
            )?;
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-b1", "orig", "anon", 1735689600i64, "tenant_b"],
            )?;
            Ok::<_, rusqlite::Error>(())
        }).await.unwrap().unwrap();

        let Json(list_a) = list_documents(
            State(state.clone()),
            Extension(auth("tenant_a")),
        ).await.expect("ok");
        let ids_a: Vec<String> = list_a["items"].as_array().unwrap()
            .iter()
            .map(|v| v["doc_id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids_a.contains(&"doc-a1".to_string()));
        assert!(!ids_a.contains(&"doc-b1".to_string()));
    }

    #[tokio::test]
    async fn test_tenant_isolation_delete() {
        let state = make_state().await;
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-c1", "orig", "anon", 1735689600i64, "tenant_c"],
            )
        }).await.unwrap().unwrap();

        let result = delete_document(
            State(state.clone()),
            Extension(auth("tenant_b")),
            Path("doc-c1".to_string()),
        ).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let status = axum::response::IntoResponse::into_response(err).status();
        assert_eq!(status.as_u16(), 404);
    }
}
