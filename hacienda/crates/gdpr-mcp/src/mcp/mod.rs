pub mod tools;

use std::sync::Arc;

use rmcp::{
    model::{CallToolResult, Content, ServerInfo},
    tool, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

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

// ── Server ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GdprServer {
    api_client: Arc<crate::api_client::ApiClient>,
}

impl GdprServer {
    pub fn new(api_client: Arc<crate::api_client::ApiClient>) -> Self {
        Self { api_client }
    }

    // ── Tools ─────────────────────────────────────────────────────────────

    /// Ingest a document: extract text, anonymize PII, and store in DB.
    ///
    /// Accepts either `text` (raw string) or `file_path` (not supported in thin-client mode).
    /// Returns the assigned `doc_id`, number of PII entities redacted, and whether
    /// L2/NER was unavailable (`ner_degraded`).
    #[tool(name = "gdpr_ingest")]
    async fn gdpr_ingest(
        &self,
        #[tool(aggr)] params: IngestParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        if params.file_path.is_some() {
            return Ok(CallToolResult::error(vec![Content::text(
                "file_path upload not supported; provide text directly"
            )]));
        }
        let text = match params.text {
            Some(t) => t,
            None => return Ok(CallToolResult::error(vec![Content::text("text field is required")])),
        };
        let legal_basis = params.legal_basis.unwrap_or_else(|| "legitimate_interest".to_string());

        match self.api_client.ingest_document(crate::api_client::IngestRequest {
            text,
            legal_basis,
            profile: None,
        }).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Search anonymized documents by keyword.
    ///
    /// Searching anonymized text ensures no raw PII is exposed in results.
    #[tool(name = "gdpr_search")]
    async fn gdpr_search(
        &self,
        #[tool(aggr)] params: SearchParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.api_client.search(crate::api_client::SearchRequest {
            query: params.query,
            limit: params.limit,
            profile: None,
        }).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Query the GDPR audit log (GDPR Art. 30 / AI Act Art. 12).
    #[tool(name = "gdpr_audit")]
    async fn gdpr_audit(
        &self,
        #[tool(aggr)] params: AuditParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let limit = params.limit.unwrap_or(50);

        match self.api_client.get_audit(limit).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Erase a document (GDPR Art. 17 right to erasure).
    #[tool(name = "gdpr_delete")]
    async fn gdpr_delete(
        &self,
        #[tool(aggr)] params: DeleteParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.api_client.delete_document(&params.document_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                format!(r#"{{"deleted":"{}"}}"#, params.document_id)
            )])),
            Err(crate::api_client::ApiError::Http { status: 404, .. }) => {
                Ok(CallToolResult::error(vec![Content::text(
                    format!("Document not found: {}", params.document_id)
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Anonymize text without storing it — useful for one-off PII scrubbing.
    #[tool(name = "gdpr_anonymize")]
    async fn gdpr_anonymize(
        &self,
        #[tool(aggr)] params: AnonymizeParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.api_client.anonymize(crate::api_client::AnonymizeRequest {
            text: params.text,
            legal_basis: "legitimate_interest".to_string(),
            ..Default::default()
        }).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// Rehydrate a previously anonymized text, restoring original PII values.
    #[tool(name = "gdpr_deanonymize")]
    async fn gdpr_deanonymize(
        &self,
        #[tool(aggr)] params: DeanonymizeParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        match self.api_client.deanonymize(crate::api_client::DeanonymizeRequest {
            text: params.text,
            session_id: params.session_id,
            ..Default::default()
        }).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// List all ingested documents with their entity counts.
    #[tool(name = "gdpr_list_documents")]
    async fn gdpr_list_documents(&self) -> Result<CallToolResult, rmcp::Error> {
        match self.api_client.list_documents().await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    /// List high-risk documents pending human review (AI Act Art. 14).
    #[tool(name = "gdpr_review_queue")]
    async fn gdpr_review_queue(
        &self,
        #[tool(aggr)] params: ReviewQueueParams,
    ) -> Result<CallToolResult, rmcp::Error> {
        let threshold = params.threshold.unwrap_or(20);
        let limit     = params.limit.unwrap_or(50);

        match self.api_client.get_review_queue(threshold, limit).await {
            Ok(r) => {
                let json = serde_json::to_string(&r)
                    .unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
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
