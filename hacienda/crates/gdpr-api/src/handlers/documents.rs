use std::sync::Arc;
use axum::extract::{Extension, State, Path};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use validator::Validate;
use crate::state::AppState;
use crate::error::{ApiError, ApiResult};
use crate::extractors::ValidJson;
use gdpr_core::pii::{AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool};
use gdpr_core::ner::chunk_text;

#[derive(Deserialize, Validate)]
pub struct IngestReq {
    #[validate(length(min = 1, max = 100_000))]
    pub text:        String,
    pub file_path:   Option<String>,   // always rejected in T5 → 400
    #[validate(length(min = 1))]
    pub legal_basis: String,           // now required
    pub profile:     Option<String>,
}

#[derive(Serialize)]
pub struct IngestResp {
    pub doc_id:              String,
    pub pii_count:           usize,
    pub ner_degraded:        bool,
    pub ai_act_risk_level:   String,
    pub chunk_count:         usize,
    pub decision_explanation: String,
}

#[derive(Serialize)]
pub struct DocList {
    pub items: Vec<serde_json::Value>,
    pub total: usize,
}

pub async fn post_document(
    State(state): State<AppState>,
    Extension(auth): Extension<crate::state::AuthContext>,
    ValidJson(req): ValidJson<IngestReq>,
) -> ApiResult<(StatusCode, Json<IngestResp>)> {
    // T5: file_path not supported
    if req.file_path.is_some() {
        return Err(ApiError::Validation(
            "file_path upload not supported; provide text directly".to_string(),
        ));
    }

    let text        = req.text;
    let legal_basis = req.legal_basis;
    let profile     = req.profile
        .as_deref()
        .and_then(|p| serde_json::from_value::<AnonProfile>(serde_json::Value::String(p.to_string())).ok())
        .unwrap_or(AnonProfile::Max);

    // Anonymize (L1 regex + optional L2 NER)
    let doc_id      = uuid::Uuid::new_v4().to_string();
    let session_id  = uuid::Uuid::new_v4().to_string();
    let mut session_ctx = SessionContext::new(profile);
    let pool_strings: Vec<String> = get_pool(&profile).iter().map(|s| s.to_string()).collect();
    let engine      = TreatmentEngine::new(pool_strings);

    let result = tokio::task::spawn_blocking(move || {
        anonymize_with_profile(&text, profile, &mut session_ctx, &engine)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("spawn_blocking: {e}")))?
    .map_err(|e| ApiError::Internal(format!("anonymize: {e}")))?;

    // Store session in cache so deanonymize works
    {
        let dm = dashmap::DashMap::new();
        for (token, original) in &result.token_map {
            dm.insert(token.clone(), original.clone());
        }
        state.session_cache.insert(session_id, crate::state::SessionCache {
            token_map: dm, created_at: std::time::Instant::now(),
        });
    }

    let pii_count    = result.pii_count;
    let ner_degraded = result.ner_degraded;
    let anon_text    = result.text;

    // Determine AI Act risk level
    let ai_act_risk_level = match pii_count {
        0        => "low",
        1..=10   => "low",
        11..=50  => "medium",
        _        => "high",
    }.to_string();
    let decision_explanation = format!(
        "Detected {} PII entities (risk: {}). NER degraded: {}.",
        pii_count, ai_act_risk_level, ner_degraded
    );

    // Chunk the anonymized text
    const CHUNK_MAX_WORDS:     usize = 400;
    const CHUNK_OVERLAP_WORDS: usize = 50;
    let chunks = chunk_text(&anon_text, CHUNK_MAX_WORDS, CHUNK_OVERLAP_WORDS);
    let chunk_count = chunks.len();

    // Persist to SQLite
    let doc_id_clone      = doc_id.clone();
    let token_map         = result.token_map;
    let ai_risk_cl        = ai_act_risk_level.clone();
    let pii_c             = pii_count as i64;
    let ner_d             = ner_degraded as i64;

    let conn = state.db.get().await
        .map_err(|e| ApiError::Internal(format!("db pool: {e}")))?;

    conn.interact(move |c| {
        // Insert document
        c.execute(
            "INSERT INTO documents (id, anon_text, pii_count, ner_degraded, created_at) VALUES (?1, ?2, ?3, ?4, unixepoch())",
            rusqlite::params![doc_id_clone, anon_text, pii_c, ner_d],
        )?;

        // Insert chunks
        for (idx, chunk) in chunks.iter().enumerate() {
            let chunk_id = uuid::Uuid::new_v4().to_string();
            c.execute(
                "INSERT INTO doc_chunks (id, doc_id, chunk_idx, chunk_text, chunk_offset, created_at) VALUES (?1, ?2, ?3, ?4, ?5, unixepoch())",
                rusqlite::params![chunk_id, doc_id_clone, idx as i64, chunk.text, chunk.offset as i64],
            )?;
        }

        // Insert entity map entries derived from token_map (pseudonym → original).
        // ProfileAnonymizeResult has no typed entity Vec; we derive entries from the
        // token map using "UNKNOWN" as entity_type and "L1" as detection_layer.
        for (pseudonym, _original) in &token_map {
            c.execute(
                "INSERT INTO doc_entity_map (document_id, entity_type, pseudonym, detection_layer, confidence, ner_degraded) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    doc_id_clone, "UNKNOWN", pseudonym,
                    "L1", 1.0f64, ner_d,
                ],
            )?;
        }
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("interact: {e}")))?
    .map_err(|e| ApiError::Internal(format!("sqlite: {e}")))?;

    // Write ClickHouse audit trail (best-effort)
    if let Some(ch) = &state.clickhouse {
        let row = crate::clients::GdprAuditRow {
            document_id:       doc_id.clone(),
            action:            "ingest".to_string(),
            pii_count_before:  pii_count as u32,
            pii_count_after:   0,
            ner_degraded:      ner_degraded as u8,
            processing_time_ms: 0,
            legal_basis:       legal_basis,
            user_id:           auth.tenant_id.clone(),
            model_version:     "t5".to_string(),
            ai_act_risk_level: ai_risk_cl.clone(),
            decision_explanation: decision_explanation.clone(),
        };
        let ch_clone = Arc::clone(ch);
        tokio::spawn(async move {
            // write_audit_row returns () — fire and forget
            ch_clone.write_audit_row(row).await;
        });
    }

    Ok((StatusCode::CREATED, Json(IngestResp {
        doc_id,
        pii_count,
        ner_degraded,
        ai_act_risk_level,
        chunk_count,
        decision_explanation,
    })))
}

pub async fn list_documents(
    State(_state): State<AppState>,
) -> ApiResult<Json<DocList>> {
    Ok(Json(DocList { items: vec![], total: 0 }))
}

pub async fn get_document(
    State(_state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"id": id, "status": "ok"})))
}

pub async fn delete_document(
    State(_state): State<AppState>,
    Path(_id): Path<String>,
) -> ApiResult<StatusCode> {
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use dashmap::DashMap;
    use crate::state::{AppState, SessionCache, VecStore, CachedKey};

    /// Build a minimal AppState backed by an in-memory SQLite pool.
    /// Runs the full migration batch so all tables exist.
    async fn test_state() -> AppState {
        let cfg  = deadpool_sqlite::Config::new(":memory:");
        let db   = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        {
            let conn = db.get().await.unwrap();
            conn.interact(|c| c.execute_batch("
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS api_keys (id TEXT PRIMARY KEY, name TEXT NOT NULL, key_hash TEXT NOT NULL, created_at INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS usage_records (id INTEGER PRIMARY KEY AUTOINCREMENT, api_key_id TEXT NOT NULL, tokens_in INTEGER NOT NULL DEFAULT 0, tokens_out INTEGER NOT NULL DEFAULT 0, month TEXT NOT NULL, created_at INTEGER NOT NULL);
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
    async fn test_post_document_stores_and_returns_fields() {
        let state = test_state().await;
        let req = IngestReq {
            text:        "Maître Dupont représente la SAS Acme pour un montant de 150 000 €.".to_string(),
            legal_basis: "consent".to_string(),
            profile:     None,
            file_path:   None,
        };
        let result = post_document(
            axum::extract::State(state.clone()),
            axum::extract::Extension(crate::state::AuthContext {
                tenant_id: "t1".to_string(), api_key_id: "k1".to_string(),
                scopes: vec!["*".to_string()], plan: gdpr_billing::Plan::Starter,
            }),
            crate::extractors::ValidJson(req),
        ).await;
        let (status, Json(resp)) = result.expect("post_document failed");
        assert_eq!(status, axum::http::StatusCode::CREATED);
        assert!(!resp.doc_id.is_empty(), "doc_id must be set");
        assert!(resp.pii_count > 0, "L1 regex should detect IBAN/name: pii_count={}", resp.pii_count);
        assert!(resp.chunk_count > 0, "chunk_count must be positive");
        assert!(!resp.decision_explanation.is_empty());

        // Verify document was stored in SQLite
        let db = state.db.get().await.unwrap();
        let count: i64 = db.interact(move |c| {
            c.query_row("SELECT COUNT(*) FROM documents WHERE id = ?1", [&resp.doc_id], |r| r.get(0))
        }).await.unwrap().unwrap();
        assert_eq!(count, 1, "document must be persisted");
    }
}
