// tools.rs — parameter and result structs for MCP tool handlers.
// All function bodies have been moved to api_client calls in mod.rs (T5 thin-client).

use serde::Serialize;

// ── anonymize_text ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AnonymizeTextResult {
    pub anonymized: String,
    pub pii_count: usize,
    pub ner_degraded: bool,
}

// ── deanonymize_response ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DeanonymizeResult {
    pub text: String,
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

// ── delete_document ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DeleteResult {
    pub document_id: String,
    pub success: bool,
    pub rows_deleted: usize,
    pub reason: String,
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

// ── audit_report ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct AuditReportResult {
    pub total_operations: usize,
    pub rows: Vec<serde_json::Value>,
    pub note: String,
}
