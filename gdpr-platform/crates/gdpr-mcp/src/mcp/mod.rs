pub mod tools;
#[allow(unused_imports)]
pub use tools::{
    anonymize_text, deanonymize_response, ingest_document, list_documents,
    delete_document, audit_report, search_documents,
};

use std::sync::Arc;

use rmcp::{
    model::{CallToolResult, Content, ServerInfo},
    tool, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use std::collections::HashMap;

use gdpr_core::{
    audit::{now_unix, AuditEvent, AuditLog},
    clients::{clickhouse::GdprAuditRow, metrics::metrics},
    extraction::extract_text,
    ner::chunk_text,
    state::CoreState,
};

const CHUNK_MAX_WORDS: usize = 400;
const CHUNK_OVERLAP_WORDS: usize = 50;

// ── Parameter structs ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IngestParams {
    /// Raw text to anonymize and store. Mutually exclusive with `file_path`.
    pub text: Option<String>,
    /// Absolute path to a file (PDF, DOCX, ODT, TXT, image).
    pub file_path: Option<String>,
    /// GDPR Art. 6 legal basis (e.g. "consent", "legitimate_interest", "legal_obligation").
    /// Defaults to "legitimate_interest" if omitted.
    pub legal_basis: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Keyword or phrase to search within anonymized documents.
    pub query: String,
    /// Maximum number of results to return (default: 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuditParams {
    /// Filter audit log by document ID (optional).
    pub doc_id: Option<String>,
    /// Maximum number of audit records to return (default: 50).
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteParams {
    /// Document ID to erase (GDPR Art. 17 right to erasure).
    pub doc_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnonymizeParams {
    /// Raw text to anonymize (does not persist to storage).
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeanonymizeParams {
    /// Previously anonymized text containing pseudo-tokens (e.g. PERSON_7).
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewQueueParams {
    /// Minimum PII count to consider a document high-risk (default: 20).
    pub threshold: Option<u32>,
    /// Maximum number of records to return (default: 50).
    pub limit: Option<u32>,
}

// ── Response structs ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct IngestResponse {
    doc_id:               String,
    pii_count:            usize,
    chunk_count:          usize,
    ner_degraded:         bool,
    ai_act_risk_level:    String,
    decision_explanation: String,
}

#[derive(Serialize)]
struct SearchHit {
    doc_id:     String,
    chunk_text: String,
    chunk_idx:  Option<usize>,
    pii_count:  i64,
    created_at: i64,
}

#[derive(Serialize)]
struct AuditRecord {
    id: i64,
    event_type: String,
    doc_id: Option<String>,
    pii_count: Option<i64>,
    detail: Option<String>,
    ts_unix: i64,
}

#[derive(Serialize)]
struct DeleteResponse {
    erased: String,
}

// ── Server ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GdprServer {
    state: Arc<CoreState>,
    audit: AuditLog,
}

impl GdprServer {
    pub fn new(state: Arc<CoreState>) -> Self {
        let audit = AuditLog::new(state.db.clone());
        Self { audit, state }
    }

    // ── Tools ─────────────────────────────────────────────────────────────

    /// Ingest a document: extract text, anonymize PII, and store in DB.
    ///
    /// Accepts either `text` (raw string) or `file_path` (PDF/DOCX/ODT/image/text file).
    /// Returns the assigned `doc_id`, number of PII entities redacted, and whether
    /// L2/NER was unavailable (`ner_degraded`).
    ///
    /// Invariant I1: L1 (cloakpipe) detection is never bypassed — failure returns an error.
    /// Invariant I2: L2/NER failure is tolerable; `ner_degraded: true` is set in the response.
    #[tool(name = "gdpr_ingest")]
    async fn gdpr_ingest(
        &self,
        #[tool(aggr)] params: IngestParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        // 1. Acquire raw text
        let raw = match (params.text, params.file_path) {
            (Some(t), _) => t,
            (None, Some(path)) => {
                let cb = Arc::clone(&self.state.kreuzberg_cb);
                match cb.call(extract_text(&path)).await {
                    Ok(t) => t,
                    Err(e) => {
                        return Ok(CallToolResult::error(vec![Content::text(format!(
                            "Extraction failed: {e}"
                        ))]))
                    }
                }
            }
            (None, None) => {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Provide either `text` or `file_path`",
                )]))
            }
        };

        // 2. Anonymize via EnginePool (CPU-bound — regex + optional ONNX).
        //    L1 failure is fatal per Invariant I1.
        let anonymize_start = std::time::Instant::now();
        let legal_basis = params.legal_basis.unwrap_or_else(|| "legitimate_interest".into());
        let pool_arc = Arc::clone(&self.state.engine_pool);
        let raw_clone = raw.clone();
        let anonymize_result = tokio::task::spawn_blocking(move || {
            let mut ner_degraded = false;
            let results = pool_arc.anonymize_batch(
                &[raw_clone.as_str()],
                &mut ner_degraded,
            )?;
            let result = results.into_iter().next()
                .ok_or_else(|| anyhow::anyhow!("empty batch result"))?;
            Ok::<_, anyhow::Error>((result, ner_degraded))
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking panic: {e}"));

        let (result, ner_degraded) = match anonymize_result {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "PII detection failed: {e}"
                ))]))
            }
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Internal error: {e}"
                ))]))
            }
        };

        // 3. Chunk anonymized text for RAG retrieval
        let doc_id = Uuid::new_v4().to_string();
        let ts = now_unix() as i64;
        let chunks = chunk_text(&result.text, CHUNK_MAX_WORDS, CHUNK_OVERLAP_WORDS);

        // 4. Store document + all chunks via deadpool interact
        let doc_id_clone  = doc_id.clone();
        let anon_text     = result.text.clone();
        let pii_count_i64 = result.pii_count as i64;
        let ner_deg_i64   = ner_degraded as i64;
        let chunks_clone  = chunks.iter()
            .map(|c| (c.text.clone(), c.offset))
            .collect::<Vec<_>>();

        let conn = self.state.db.get().await
            .map_err(|e| anyhow::anyhow!("db pool: {e}"));
        let conn = match conn {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("DB pool error: {e}"))])),
        };

        let store_result: Result<Vec<(String, String, usize)>, anyhow::Error> = conn.interact(move |c| {
            c.execute(
                "INSERT INTO documents (id, anon_text, pii_count, ner_degraded, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    doc_id_clone,
                    anon_text,
                    pii_count_i64,
                    ner_deg_i64,
                    ts
                ],
            )?;
            let mut chunk_tuples = Vec::with_capacity(chunks_clone.len());
            for (chunk_idx, (chunk_text, chunk_offset)) in chunks_clone.iter().enumerate() {
                let chunk_id = Uuid::new_v4().to_string();
                c.execute(
                    "INSERT INTO doc_chunks (id, doc_id, chunk_idx, chunk_text, chunk_offset, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        chunk_id,
                        doc_id_clone,
                        chunk_idx as i64,
                        chunk_text,
                        *chunk_offset as i64,
                        ts
                    ],
                )?;
                chunk_tuples.push((chunk_id, chunk_text.clone(), chunk_idx));
            }
            Ok::<_, rusqlite::Error>(chunk_tuples)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("db write: {e}")));

        let chunk_tuples = match store_result {
            Ok(t) => t,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "DB write failed: {e}"
                ))]))
            }
        };
        let chunk_count = chunk_tuples.len();

        // 5. Audit trail (GDPR Art. 30) — written after successful store
        if let Err(e) = self.audit
            .record(AuditEvent::Ingest { doc_id: &doc_id, pii_count: result.pii_count })
            .await
        {
            tracing::warn!(doc_id = %doc_id, "audit record failed: {e}");
        }

        // 6. Prometheus metrics
        {
            let m = metrics();
            for entity in &result.entities {
                let layer = format!("{:?}", entity.source).to_lowercase();
                m.pii_detected.with_label_values(&[&layer]).inc();
            }
            m.documents_ingested.inc();
            if ner_degraded {
                m.ner_degraded.inc();
            }
            m.detector_latency_ms
                .observe(anonymize_start.elapsed().as_millis() as f64);
            m.cb_state
                .with_label_values(&["kreuzberg"])
                .set(if self.state.kreuzberg_cb.is_open() { 1.0 } else { 0.0 });
        }

        // AI Act Art. 9 + 13: risk classification and transparency explanation
        let risk_level  = ai_act_risk_level(result.pii_count);
        let explanation = decision_explanation(&result.entities, ner_degraded);

        // ClickHouse audit trail (GDPR Art. 30) — fire-and-forget, non-blocking
        if let Some(ch) = &self.state.clickhouse {
            let ch   = Arc::clone(ch);
            let d_id = doc_id.clone();
            let pc   = result.pii_count as u32;
            let nd   = ner_degraded;
            let ms   = anonymize_start.elapsed().as_millis() as u32;
            let rl   = risk_level.to_string();
            let ex   = explanation.clone();
            let lb   = legal_basis.clone();
            tokio::spawn(async move {
                ch.record(GdprAuditRow {
                    document_id:          d_id,
                    action:               "ingest".into(),
                    pii_count_before:     pc,
                    pii_count_after:      0,
                    ner_degraded:         nd,
                    processing_time_ms:   ms,
                    legal_basis:          lb,
                    user_id:              String::new(),
                    model_version:        "gliner-pii-edge-v1.0".into(),
                    ai_act_risk_level:    rl,
                    decision_explanation: ex,
                }).await;
            });
        }

        // VecStore chunk upsert — fire-and-forget; failure is non-fatal.
        if let Some(vs) = self.state.vec_store.as_ref().map(Arc::clone) {
            let d_id   = doc_id.clone();
            let tuples = chunk_tuples.clone();
            tokio::spawn(async move {
                if let Err(e) = vs.upsert_chunks(&d_id, &tuples).await {
                    tracing::warn!(doc_id = %d_id, "VecStore chunk upsert failed: {e}");
                }
            });
        }

        let resp = IngestResponse {
            doc_id,
            pii_count: result.pii_count,
            chunk_count,
            ner_degraded,
            ai_act_risk_level: risk_level.to_string(),
            decision_explanation: explanation,
        };
        let json = serde_json::to_string(&resp).unwrap_or_else(|e| {
            format!(r#"{{"error":"serialization failed: {e}"}}"#)
        });
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Search anonymized documents by keyword (LIKE match on anonymized text).
    ///
    /// Searching anonymized text ensures no raw PII is exposed in results.
    /// Returns matching document IDs with chunk text.
    ///
    /// Note: search queries are always recorded in the audit log, regardless of outcome.
    #[tool(name = "gdpr_search")]
    async fn gdpr_search(
        &self,
        #[tool(aggr)] params: SearchParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let limit = params.limit.unwrap_or(10) as i64;

        // Audit query regardless of result (search intent is always loggable)
        if let Err(e) = self.audit.record(AuditEvent::Search { query: &params.query }).await {
            tracing::warn!("audit record failed: {e}");
        }

        // Vector search (VecStore) preferred path
        if let Some(vs) = self.state.vec_store.as_ref().map(Arc::clone) {
            match vs.search(&params.query, limit as usize).await {
                Ok(hits) => {
                    let results: Vec<SearchHit> = hits
                        .into_iter()
                        .map(|h| SearchHit {
                            doc_id:     h.doc_id,
                            chunk_text: h.chunk_text.unwrap_or_default(),
                            chunk_idx:  h.chunk_idx,
                            pii_count:  h.pii_count,
                            created_at: h.created_at,
                        })
                        .collect();
                    let json = serde_json::to_string(&results).unwrap_or_else(|e| {
                        format!(r#"{{"error":"serialization failed: {e}"}}"#)
                    });
                    return Ok(CallToolResult::success(vec![Content::text(json)]));
                }
                Err(e) => {
                    tracing::warn!("VecStore search failed, falling back to SQLite LIKE: {e}");
                }
            }
        }

        // SQLite LIKE fallback
        let escaped = params
            .query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let query_clone = params.query.clone();

        let conn = self.state.db.get().await
            .map_err(|e| anyhow::anyhow!("db pool: {e}"));
        let conn = match conn {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("DB pool error: {e}"))])),
        };

        let hits_result: Result<Vec<SearchHit>, anyhow::Error> = conn.interact(move |c| {
            // Primary: search in doc_chunks
            let mut stmt = c.prepare(
                "SELECT dc.doc_id, dc.chunk_text, dc.chunk_idx, d.pii_count, d.created_at
                 FROM doc_chunks dc
                 JOIN documents d ON dc.doc_id = d.id
                 WHERE dc.chunk_text LIKE ?1 ESCAPE '\\' LIMIT ?2",
            )?;

            let hits: Vec<SearchHit> = stmt
                .query_map(rusqlite::params![pattern, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .map(|(doc_id, chunk_text, chunk_idx, pii_count, created_at)| SearchHit {
                    doc_id,
                    chunk_text,
                    chunk_idx: Some(chunk_idx as usize),
                    pii_count,
                    created_at,
                })
                .collect();

            if !hits.is_empty() {
                return Ok::<_, rusqlite::Error>(hits);
            }

            // Backward compat: whole-document fallback
            let escaped2 = query_clone.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let pattern2 = format!("%{escaped2}%");
            let mut stmt2 = c.prepare(
                "SELECT id, anon_text, pii_count, created_at
                 FROM documents WHERE anon_text LIKE ?1 ESCAPE '\\' LIMIT ?2",
            )?;
            let fallback: Vec<SearchHit> = stmt2
                .query_map(rusqlite::params![pattern2, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .map(|(doc_id, text, pii_count, created_at)| {
                    let chunk_text = extract_snippet(&text, &query_clone, 50);
                    SearchHit { doc_id, chunk_text, chunk_idx: None, pii_count, created_at }
                })
                .collect();
            Ok::<_, rusqlite::Error>(fallback)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("search query: {e}")));

        match hits_result {
            Ok(hits) => {
                let json = serde_json::to_string(&hits)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Search failed: {e}"))])),
        }
    }

    /// Query the GDPR audit log (GDPR Art. 30 / AI Act Art. 12).
    ///
    /// Returns a chronological record of ingest, search, and delete events.
    /// Filter by `doc_id` to trace the full lifecycle of a specific document.
    #[tool(name = "gdpr_audit")]
    async fn gdpr_audit(
        &self,
        #[tool(aggr)] params: AuditParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let limit = params.limit.unwrap_or(50) as i64;

        if let Err(e) = self.audit.record(AuditEvent::AuditQuery { doc_id: params.doc_id.as_deref() }).await {
            tracing::warn!("audit record failed: {e}");
        }

        let filter_doc_id = params.doc_id.clone();
        let conn = self.state.db.get().await
            .map_err(|e| anyhow::anyhow!("db pool: {e}"));
        let conn = match conn {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("DB pool error: {e}"))])),
        };

        let records_result: Result<Vec<AuditRecord>, anyhow::Error> = conn.interact(move |c| {
            let records = if let Some(ref doc_id) = filter_doc_id {
                let mut stmt = c.prepare(
                    "SELECT id, event_type, doc_id, pii_count, detail, ts_unix
                     FROM audit_log WHERE doc_id = ?1 ORDER BY id DESC LIMIT ?2",
                )?;
                collect_audit_records_sync(&mut stmt, rusqlite::params![doc_id, limit])
            } else {
                let mut stmt = c.prepare(
                    "SELECT id, event_type, doc_id, pii_count, detail, ts_unix
                     FROM audit_log ORDER BY id DESC LIMIT ?1",
                )?;
                collect_audit_records_sync(&mut stmt, rusqlite::params![limit])
            };
            Ok::<_, rusqlite::Error>(records)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("audit query: {e}")));

        match records_result {
            Ok(records) => {
                let json = serde_json::to_string(&records)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Audit query failed: {e}"))])),
        }
    }

    /// Erase a document (GDPR Art. 17 right to erasure).
    ///
    /// Permanently deletes the anonymized text and metadata from the document store.
    /// The audit record is written only after the deletion succeeds, so the log
    /// accurately reflects what actually happened.
    #[tool(name = "gdpr_delete")]
    async fn gdpr_delete(
        &self,
        #[tool(aggr)] params: DeleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let doc_id = params.doc_id.clone();

        let conn = self.state.db.get().await
            .map_err(|e| anyhow::anyhow!("db pool: {e}"));
        let conn = match conn {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("DB pool error: {e}"))])),
        };

        let doc_id_clone = doc_id.clone();
        let delete_result: Result<(usize, Vec<String>), anyhow::Error> = conn.interact(move |c| {
            // Collect chunk IDs before deletion (needed for VecStore cleanup)
            let chunk_ids: Vec<String> = c
                .prepare("SELECT id FROM doc_chunks WHERE doc_id = ?1")?
                .query_map(rusqlite::params![doc_id_clone], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();

            let rows_deleted = c.execute(
                "DELETE FROM documents WHERE id = ?1",
                rusqlite::params![doc_id_clone],
            )?;

            if rows_deleted > 0 {
                let _ = c.execute(
                    "DELETE FROM doc_chunks WHERE doc_id = ?1",
                    rusqlite::params![doc_id_clone],
                );
            }

            Ok::<_, rusqlite::Error>((rows_deleted, chunk_ids))
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("delete: {e}")));

        match delete_result {
            Ok((0, _)) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Document not found: {doc_id}"
            ))])),
            Ok((_, chunk_ids)) => {
                // Also erase entity map rows (GDPR Art. 17 completeness)
                if let Err(e) = self.state.doc_audit.delete_document(&doc_id).await {
                    tracing::warn!("doc_audit delete failed: {e}");
                }
                // Remove chunk embeddings from VecStore
                if let Some(vs) = &self.state.vec_store {
                    if let Err(e) = vs.delete_by_ids(&chunk_ids).await {
                        tracing::warn!("VecStore chunk delete failed: {e}");
                    }
                }
                // Audit after confirmed delete (only record what actually happened)
                if let Err(e) = self.audit.record(AuditEvent::Delete { doc_id: &doc_id }).await {
                    tracing::warn!("audit record failed: {e}");
                }
                metrics().deletes.inc();
                // ClickHouse audit trail — fire-and-forget
                if let Some(ch) = &self.state.clickhouse {
                    let ch   = Arc::clone(ch);
                    let d_id = doc_id.clone();
                    tokio::spawn(async move {
                        ch.record(GdprAuditRow {
                            document_id:          d_id,
                            action:               "delete".into(),
                            pii_count_before:     0,
                            pii_count_after:      0,
                            ner_degraded:         false,
                            processing_time_ms:   0,
                            legal_basis:          "legal_obligation".into(),
                            user_id:              String::new(),
                            model_version:        String::new(),
                            ai_act_risk_level:    "low".into(),
                            decision_explanation: String::new(),
                        }).await;
                    });
                }
                let json = serde_json::to_string(&DeleteResponse { erased: doc_id })
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Delete failed: {e}"
            ))])),
        }
    }

    /// Anonymize text without storing it — useful for one-off PII scrubbing.
    ///
    /// The text is processed through L1 (regex) and optionally L2 (NER),
    /// but is NOT persisted to the document store. Use `gdpr_ingest` to store.
    #[tool(name = "gdpr_anonymize")]
    async fn gdpr_anonymize(
        &self,
        #[tool(aggr)] params: AnonymizeParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match tools::anonymize_text(&self.state, params.text).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Rehydrate a previously anonymized text, restoring original PII values.
    ///
    /// Replaces pseudo-tokens (e.g. `PERSON_7`, `IBAN_3`) with the originals
    /// from the AES-256-GCM vault. Only works within the same vault session.
    #[tool(name = "gdpr_deanonymize")]
    async fn gdpr_deanonymize(
        &self,
        #[tool(aggr)] params: DeanonymizeParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match tools::deanonymize_response(&self.state, params.text).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// List all ingested documents with their entity counts.
    ///
    /// Returns a summary from the `doc_entity_map` table — does not expose
    /// anonymized text or original PII values.
    #[tool(name = "gdpr_list_documents")]
    async fn gdpr_list_documents(&self) -> Result<CallToolResult, rmcp::Error> {
        match tools::list_documents(&self.state).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// List high-risk documents pending human review (AI Act Art. 14).
    ///
    /// Returns documents whose PII count meets or exceeds `threshold` (default: 20).
    /// Human oversight is required for these documents before further processing.
    #[tool(name = "gdpr_review_queue")]
    async fn gdpr_review_queue(
        &self,
        #[tool(aggr)] params: ReviewQueueParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let threshold = params.threshold.unwrap_or(20) as i64;
        let limit     = params.limit.unwrap_or(50) as i64;

        let conn = self.state.db.get().await
            .map_err(|e| anyhow::anyhow!("db pool: {e}"));
        let conn = match conn {
            Ok(c) => c,
            Err(e) => return Ok(CallToolResult::error(vec![Content::text(format!("DB pool error: {e}"))])),
        };

        #[derive(Serialize)]
        struct ReviewItem {
            doc_id: String,
            pii_count: i64,
            ner_degraded: bool,
            created_at: i64,
            ai_act_risk_level: String,
        }

        let items_result: Result<Vec<ReviewItem>, anyhow::Error> = conn.interact(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, pii_count, ner_degraded, created_at
                 FROM documents WHERE pii_count >= ?1 ORDER BY pii_count DESC LIMIT ?2",
            )?;

            let items: Vec<ReviewItem> = stmt
                .query_map(rusqlite::params![threshold, limit], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)? != 0,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .map(|(doc_id, pii_count, ner_degraded, created_at)| ReviewItem {
                    ai_act_risk_level: ai_act_risk_level(pii_count as usize).to_string(),
                    doc_id,
                    pii_count,
                    ner_degraded,
                    created_at,
                })
                .collect();
            Ok::<_, rusqlite::Error>(items)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))
        .and_then(|r| r.map_err(|e| anyhow::anyhow!("review_queue: {e}")));

        match items_result {
            Ok(items) => {
                let json = serde_json::to_string(&items)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!("Review queue failed: {e}"))])),
        }
    }

    rmcp::tool_box!(GdprServer {
        gdpr_ingest,
        gdpr_search,
        gdpr_audit,
        gdpr_delete,
        gdpr_anonymize,
        gdpr_deanonymize,
        gdpr_list_documents,
        gdpr_review_queue
    });
}

// ── ServerHandler ────────────────────────────────────────────────────────────

impl ServerHandler for GdprServer {
    rmcp::tool_box!(@derive);

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            server_info: rmcp::model::Implementation {
                name: "gdpr-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "GDPR-compliant document anonymization MCP server. \
                 All PII is redacted before storage. \
                 Lifecycle: gdpr_ingest → gdpr_search → gdpr_delete."
                    .into(),
            ),
            ..Default::default()
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// AI Act Art. 9 risk classification based on PII entity count.
fn ai_act_risk_level(pii_count: usize) -> &'static str {
    match pii_count {
        0       => "low",
        1..=9   => "medium",
        10..=29 => "high",
        _       => "critical",
    }
}

/// AI Act Art. 13 transparency: human-readable summary of detected PII.
fn decision_explanation(entities: &[cloakpipe_core::DetectedEntity], ner_degraded: bool) -> String {
    if entities.is_empty() {
        return "No PII detected".into();
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for e in entities {
        *counts.entry(format!("{:?}", e.category)).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(cat, n)| format!("{n}×{cat}"))
        .collect();
    parts.sort();
    let ner_label = if ner_degraded { "L1 only" } else { "L1+L2" };
    format!("Detected {} PII entities via {}: {}", entities.len(), ner_label, parts.join(", "))
}

/// Return a short snippet of `text` centred around the first occurrence of `needle`.
///
/// Uses char-boundary-safe slicing to avoid panics on multi-byte UTF-8 text.
fn extract_snippet(text: &str, needle: &str, radius: usize) -> String {
    let lower_text   = text.to_lowercase();
    let lower_needle = needle.to_lowercase();

    let Some(byte_pos) = lower_text.find(&lower_needle) else {
        return text.chars().take(radius * 2).collect();
    };

    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let match_end_byte = byte_pos + lower_needle.len();

    let char_idx   = boundaries.partition_point(|&b| b < byte_pos);
    let start_char = char_idx.saturating_sub(radius);
    let end_char   = (char_idx + lower_needle.chars().count() + radius).min(boundaries.len());

    let start_byte = boundaries[start_char];
    let end_byte   = boundaries.get(end_char).copied().unwrap_or(text.len());
    let end_byte   = end_byte.max(match_end_byte).min(text.len());

    format!("…{}…", &text[start_byte..end_byte])
}

fn collect_audit_records_sync(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Vec<AuditRecord> {
    stmt.query_map(params, |row| {
        Ok(AuditRecord {
            id:         row.get(0)?,
            event_type: row.get(1)?,
            doc_id:     row.get(2)?,
            pii_count:  row.get(3)?,
            detail:     row.get(4)?,
            ts_unix:    row.get(5)?,
        })
    })
    .into_iter()
    .flatten()
    .filter_map(|r| r.ok())
    .collect()
}
