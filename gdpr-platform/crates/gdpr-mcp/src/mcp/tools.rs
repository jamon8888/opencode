//! Thin async functions wrapping CoreState — testable without MCP transport.

use serde::Serialize;
use uuid::Uuid;

use gdpr_core::{
    audit::{now_unix, AuditEvent, AuditLog, DocEntityRow},
    error::{GdprError, Result},
    extraction::extract_text,
    state::CoreState,
};

// ── anonymize_text ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AnonymizeTextResult {
    pub anonymized: String,
    pub pii_count: usize,
    pub ner_degraded: bool,
}

pub async fn anonymize_text(state: &CoreState, text: String) -> Result<AnonymizeTextResult> {
    let pool_arc = std::sync::Arc::clone(&state.engine_pool);
    let (anonymized, pii_count, ner_degraded) = tokio::task::spawn_blocking(move || {
        let mut ner_degraded = vec![false];
        let results = pool_arc
            .anonymize_batch(&[text.as_str()], &mut ner_degraded)
            .map_err(|e| GdprError::PiiDetection(e))?;
        let r = results.into_iter().next()
            .ok_or_else(|| GdprError::Storage("empty batch result".into()))?;
        Ok::<_, GdprError>((r.text, r.pii_count, ner_degraded.first().copied().unwrap_or(false)))
    })
    .await
    .map_err(|e| GdprError::Storage(format!("spawn_blocking panic: {e}")))?
    .map_err(|e: GdprError| e)?;

    Ok(AnonymizeTextResult { anonymized, pii_count, ner_degraded })
}

// ── deanonymize_response ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DeanonymizeResult {
    pub text: String,
}

pub async fn deanonymize_response(state: &CoreState, text: String) -> Result<DeanonymizeResult> {
    let pool_arc = std::sync::Arc::clone(&state.engine_pool);
    let rehydrated = tokio::task::spawn_blocking(move || {
        pool_arc.rehydrate_text(&text)
    })
    .await
    .map_err(|e| GdprError::Storage(format!("spawn_blocking panic: {e}")))?;
    Ok(DeanonymizeResult { text: rehydrated })
}

// ── ingest_document ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct IngestResult {
    pub document_id: String,
    pub pii_count: usize,
    pub ner_degraded: bool,
    pub anonymized_preview: String,
    pub quality_score: f64,
}

pub async fn ingest_document(
    state: &CoreState,
    path: String,
    _language: String,
    _legal_basis: String,
) -> Result<IngestResult> {
    // 1. Extract raw text
    let raw = extract_text(&path)
        .await
        .map_err(|e| GdprError::Extraction(e.to_string()))?;

    // 2. Anonymize via EnginePool
    let pool_arc = std::sync::Arc::clone(&state.engine_pool);
    let (result, ner_degraded) = tokio::task::spawn_blocking(move || {
        let mut nd = vec![false];
        let mut results = pool_arc
            .anonymize_batch(&[raw.as_str()], &mut nd)
            .map_err(|e| GdprError::PiiDetection(e))?;
        let r = results.into_iter().next()
            .ok_or_else(|| GdprError::Storage("empty batch result".into()))?;
        Ok::<_, GdprError>((r, nd.first().copied().unwrap_or(false)))
    })
    .await
    .map_err(|e| GdprError::Storage(format!("spawn_blocking panic: {e}")))?
    .map_err(|e: GdprError| e)?;

    // 3. Store anonymized text in documents table
    let doc_id = Uuid::new_v4().to_string();
    let ts = now_unix() as i64;

    {
        let conn = state.db.get().await.map_err(|e| GdprError::Storage(e.to_string()))?;
        let doc_id_cl  = doc_id.clone();
        let anon_text  = result.text.clone();
        let pii_cnt    = result.pii_count as i64;
        let ner_deg    = ner_degraded as i64;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO documents (id, anon_text, pii_count, ner_degraded, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![doc_id_cl, anon_text, pii_cnt, ner_deg, ts],
            )
        })
        .await
        .map_err(|e| GdprError::Storage(format!("interact: {e}")))?
        .map_err(|e| GdprError::Storage(e.to_string()))?;
    }

    // 4. Record entities in doc_entity_map
    let entity_rows: Vec<DocEntityRow> = result
        .entities
        .iter()
        .map(|e| DocEntityRow {
            entity_type: format!("{:?}", e.category),
            pseudonym: e.original.clone(),
            detection_layer: format!("{:?}", e.source).to_lowercase(),
            confidence: Some(e.confidence),
            ner_degraded,
        })
        .collect();
    state
        .doc_audit
        .record_entities(&doc_id, entity_rows)
        .await
        .map_err(|e| GdprError::Storage(e.to_string()))?;

    // 5. Audit log
    AuditLog::new(state.db.clone())
        .record(AuditEvent::Ingest { doc_id: &doc_id, pii_count: result.pii_count })
        .await
        .map_err(|e| GdprError::Storage(e.to_string()))?;

    let preview: String = result.text.chars().take(500).collect();
    Ok(IngestResult {
        document_id: doc_id,
        pii_count: result.pii_count,
        ner_degraded,
        anonymized_preview: preview,
        quality_score: 1.0,
    })
}

// ── list_documents ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DocumentEntry {
    pub document_id: String,
    pub entity_count: usize,
}

#[derive(Serialize)]
pub struct ListDocumentsResult {
    pub documents: Vec<DocumentEntry>,
    pub total: usize,
}

pub async fn list_documents(state: &CoreState) -> Result<ListDocumentsResult> {
    let docs = state
        .doc_audit
        .list_documents()
        .await
        .map_err(|e| GdprError::Storage(e.to_string()))?;
    let total = docs.len();
    let documents = docs
        .into_iter()
        .map(|d| DocumentEntry { document_id: d.document_id, entity_count: d.entity_count })
        .collect();
    Ok(ListDocumentsResult { documents, total })
}

// ── delete_document ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DeleteResult {
    pub document_id: String,
    pub success: bool,
    pub rows_deleted: usize,
    pub reason: String,
}

/// GDPR Art. 17 Right to Erasure.
pub async fn delete_document(
    state: &CoreState,
    document_id: String,
    reason: String,
) -> Result<DeleteResult> {
    // Remove from doc_entity_map
    let rows_deleted = state
        .doc_audit
        .delete_document(&document_id)
        .await
        .map_err(|e| GdprError::Storage(e.to_string()))?;

    // Remove from documents table
    {
        let conn = state.db.get().await.map_err(|e| GdprError::Storage(e.to_string()))?;
        let doc_id_cl = document_id.clone();
        let _ = conn.interact(move |c| {
            c.execute(
                "DELETE FROM documents WHERE id = ?1",
                rusqlite::params![doc_id_cl],
            )
        })
        .await;
    }

    // Audit log
    AuditLog::new(state.db.clone())
        .record(AuditEvent::Delete { doc_id: &document_id })
        .await
        .map_err(|e| GdprError::Storage(e.to_string()))?;

    Ok(DeleteResult { document_id, success: true, rows_deleted, reason })
}

// ── search_documents ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SearchHit {
    pub document_id: String,
    pub chunk_text:  String,
    pub chunk_idx:   Option<usize>,
    pub pii_count:   i64,
    pub created_at:  i64,
}

#[derive(Serialize)]
pub struct SearchDocumentsResult {
    pub hits: Vec<SearchHit>,
}

/// Search anonymized documents by keyword.
///
/// Uses a SQLite LIKE match on the anonymized text so no raw PII is ever exposed.
pub async fn search_documents(
    state: &CoreState,
    query: String,
    limit: Option<u32>,
) -> Result<SearchDocumentsResult> {
    let limit = limit.unwrap_or(10) as i64;

    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let query_cl = query.clone();

    let conn = state.db.get().await.map_err(|e| GdprError::Storage(e.to_string()))?;

    let hits: Vec<SearchHit> = conn.interact(move |c| {
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
            .map(|(document_id, chunk_text, chunk_idx, pii_count, created_at)| SearchHit {
                document_id,
                chunk_text,
                chunk_idx: Some(chunk_idx as usize),
                pii_count,
                created_at,
            })
            .collect();

        if !hits.is_empty() {
            return Ok::<_, rusqlite::Error>(hits);
        }

        // Fallback: whole-document LIKE search
        let escaped2 = query_cl.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
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
            .map(|(document_id, text, pii_count, created_at)| {
                let chunk_text = extract_snippet(&text, &query_cl, 50);
                SearchHit { document_id, chunk_text, chunk_idx: None, pii_count, created_at }
            })
            .collect();
        Ok::<_, rusqlite::Error>(fallback)
    })
    .await
    .map_err(|e| GdprError::Storage(format!("interact: {e}")))?
    .map_err(|e| GdprError::Storage(e.to_string()))?;

    Ok(SearchDocumentsResult { hits })
}

// ── audit_report ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AuditReportResult {
    pub total_operations: usize,
    pub rows: Vec<serde_json::Value>,
    pub note: String,
}

/// Query the audit log with optional document ID and date range filters.
pub async fn audit_report(
    state: &CoreState,
    doc_id: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
) -> Result<AuditReportResult> {
    let date_from_unix = date_from.as_deref().and_then(date_str_to_unix);
    let date_to_unix   = date_to.as_deref().and_then(date_str_to_unix);

    let conn = state.db.get().await.map_err(|e| GdprError::Storage(e.to_string()))?;

    let rows: Vec<serde_json::Value> = conn.interact(move |c| {
        let mut sql =
            "SELECT id, event_type, doc_id, pii_count, detail, ts_unix \
             FROM audit_log WHERE 1=1"
                .to_string();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref id) = doc_id {
            sql.push_str(" AND doc_id = ?");
            params.push(Box::new(id.clone()));
        }
        if let Some(ts) = date_from_unix {
            sql.push_str(" AND ts_unix >= ?");
            params.push(Box::new(ts));
        }
        if let Some(ts) = date_to_unix {
            sql.push_str(" AND ts_unix <= ?");
            params.push(Box::new(ts));
        }
        sql.push_str(" ORDER BY id DESC LIMIT 200");

        let ref_params: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let mut stmt = c.prepare(&sql)?;
        let rows: Vec<serde_json::Value> = stmt
            .query_map(ref_params.as_slice(), |row| {
                let id: i64               = row.get(0)?;
                let event_type: String    = row.get(1)?;
                let row_doc_id: Option<String> = row.get(2)?;
                let pii_count: Option<i64>     = row.get(3)?;
                let detail: Option<String>     = row.get(4)?;
                let ts_unix: i64               = row.get(5)?;
                Ok((id, event_type, row_doc_id, pii_count, detail, ts_unix))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, event_type, row_doc_id, pii_count, detail, ts_unix)| {
                serde_json::json!({
                    "id": id,
                    "event_type": event_type,
                    "doc_id": row_doc_id,
                    "pii_count": pii_count,
                    "detail": detail,
                    "ts_unix": ts_unix,
                })
            })
            .collect();
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| GdprError::Storage(format!("interact: {e}")))?
    .map_err(|e| GdprError::Storage(e.to_string()))?;

    Ok(AuditReportResult {
        total_operations: rows.len(),
        rows,
        note: "audit_log (deadpool-sqlite)".to_string(),
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a `YYYY-MM-DD` date string into a Unix timestamp (seconds since epoch).
fn date_str_to_unix(s: &str) -> Option<i64> {
    let date_part = s.get(..10)?;
    let mut parts = date_part.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400)
}

/// Return a short snippet of `text` centred around the first occurrence of `needle`.
fn extract_snippet(text: &str, needle: &str, radius: usize) -> String {
    let lower_text   = text.to_lowercase();
    let lower_needle = needle.to_lowercase();

    let Some(byte_pos) = lower_text.find(&lower_needle) else {
        return text.chars().take(radius * 2).collect();
    };

    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let match_end_byte = byte_pos + lower_needle.len();
    let char_idx = boundaries.partition_point(|&b| b < byte_pos);

    let start_char = char_idx.saturating_sub(radius);
    let end_char   = (char_idx + lower_needle.chars().count() + radius).min(boundaries.len());

    let start_byte = boundaries[start_char];
    let end_byte   = boundaries.get(end_char).copied().unwrap_or(text.len());
    let end_byte   = end_byte.max(match_end_byte).min(text.len());

    format!("…{}…", &text[start_byte..end_byte])
}
