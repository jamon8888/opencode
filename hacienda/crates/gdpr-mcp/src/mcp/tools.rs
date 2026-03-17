// tools.rs — parameter and result structs for MCP tool handlers.
// All function bodies have been moved to api_client calls in mod.rs (T5 thin-client).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    pub document_id: String,
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
    /// Session ID returned by a prior anonymize call (optional).
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReviewQueueParams {
    /// Minimum PII count to consider a document high-risk (default: 20).
    pub threshold: Option<u32>,
    /// Maximum number of records to return (default: 50).
    pub limit: Option<u32>,
}

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
