// api_client.rs — typed HTTP client for gdpr-api
// All types live here. Field names match gdpr-api JSON serialization exactly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("gdpr-api error {status}: {detail}")]
    Http { status: u16, detail: String },
    #[error("gdpr-api unavailable: {0}")]
    Network(String),
}

// ── Request / Response types ──────────────────────────────────────────────────

#[derive(Serialize, Default)]
pub struct AnonymizeRequest {
    pub text:        String,
    pub legal_basis: String,
    pub profile:     Option<String>,
    pub session_id:  Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct AnonymizeResponse {
    pub anonymized_text: String,
    pub session_id:      String,
    pub pii_count:       usize,
}

#[derive(Serialize, Default)]
pub struct DeanonymizeRequest {
    pub text:       String,
    pub session_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct DeanonymizeResponse {
    pub text: String,
}

#[derive(Serialize)]
pub struct IngestRequest {
    pub text:        String,
    pub legal_basis: String,
    pub profile:     Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct IngestResponse {
    pub doc_id:               String,
    pub pii_count:            usize,
    pub ner_degraded:         bool,
    pub ai_act_risk_level:    String,
    pub chunk_count:          usize,
    pub decision_explanation: String,
}

#[derive(Deserialize, Debug)]
pub struct DocEntry {
    pub doc_id:       String,
    pub created_at:   i64,
    pub entity_count: usize,
}

#[derive(Deserialize, Debug)]
pub struct ListDocumentsResponse {
    pub documents: Vec<DocEntry>,
}

#[derive(Serialize)]
pub struct SearchRequest {
    pub query:   String,
    pub limit:   Option<u32>,
    pub profile: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct SearchResult {
    pub doc_id: String,
    pub chunk:  String,
    pub score:  f64,
}

#[derive(Deserialize, Debug)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

#[derive(Deserialize, Debug)]
pub struct AuditEntry {
    pub document_id:          String,
    pub action:               String,
    pub pii_count_before:     u32,
    pub pii_count_after:      u32,
    pub ner_degraded:         u8,
    pub processing_time_ms:   u32,
    pub legal_basis:          String,
    pub user_id:              String,
    pub model_version:        String,
    pub ai_act_risk_level:    String,
    pub decision_explanation: String,
    pub ts_unix:              u64,
}

#[derive(Deserialize, Debug)]
pub struct AuditResponse {
    pub events: Vec<AuditEntry>,
}

#[derive(Deserialize, Debug)]
pub struct ReviewEntry {
    pub doc_id:       String,
    pub entity_count: usize,
    pub created_at:   i64,
}

#[derive(Deserialize, Debug)]
pub struct ReviewQueueResponse {
    pub documents: Vec<ReviewEntry>,
}

// ── ApiClient ─────────────────────────────────────────────────────────────────

pub struct ApiClient {
    client:   reqwest::Client,
    base_url: String,
    api_key:  String,
}

impl ApiClient {
    pub fn from_env() -> Self {
        let base_url = std::env::var("GDPR_API_URL")
            .unwrap_or_else(|_| panic!("T5: GDPR_API_URL must be set"));
        let api_key = std::env::var("GDPR_API_KEY")
            .unwrap_or_else(|_| panic!("T5: GDPR_API_KEY must be set"));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        Self { client, base_url, api_key }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    async fn parse_error(resp: reqwest::Response) -> ApiError {
        let status = resp.status().as_u16();
        let detail = resp.json::<serde_json::Value>().await
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown error".to_string());
        ApiError::Http { status, detail }
    }

    pub async fn anonymize(&self, req: AnonymizeRequest) -> Result<AnonymizeResponse, ApiError> {
        let resp = self.client
            .post(format!("{}/v1/anonymize", self.base_url))
            .header("Authorization", self.auth_header())
            .json(&req)
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<AnonymizeResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn deanonymize(&self, req: DeanonymizeRequest) -> Result<DeanonymizeResponse, ApiError> {
        let resp = self.client
            .post(format!("{}/v1/deanonymize", self.base_url))
            .header("Authorization", self.auth_header())
            .json(&req)
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<DeanonymizeResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn ingest_document(&self, req: IngestRequest) -> Result<IngestResponse, ApiError> {
        let resp = self.client
            .post(format!("{}/v1/documents", self.base_url))
            .header("Authorization", self.auth_header())
            .json(&req)
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<IngestResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn list_documents(&self) -> Result<ListDocumentsResponse, ApiError> {
        let resp = self.client
            .get(format!("{}/v1/documents", self.base_url))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<ListDocumentsResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn delete_document(&self, doc_id: &str) -> Result<(), ApiError> {
        let resp = self.client
            .delete(format!("{}/v1/documents/{}", self.base_url, doc_id))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        Ok(())
    }

    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, ApiError> {
        let resp = self.client
            .post(format!("{}/v1/search", self.base_url))
            .header("Authorization", self.auth_header())
            .json(&req)
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<SearchResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn get_audit(&self, limit: u32) -> Result<AuditResponse, ApiError> {
        let resp = self.client
            .get(format!("{}/v1/audit?limit={}", self.base_url, limit))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<AuditResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }

    pub async fn get_review_queue(&self, threshold: u32, limit: u32) -> Result<ReviewQueueResponse, ApiError> {
        let resp = self.client
            .get(format!("{}/v1/review-queue?threshold={}&limit={}", self.base_url, threshold, limit))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| if e.is_timeout() {
                ApiError::Network("request timed out after 30s".to_string())
            } else {
                ApiError::Network(e.to_string())
            })?;

        if !resp.status().is_success() {
            return Err(Self::parse_error(resp).await);
        }
        resp.json::<ReviewQueueResponse>().await
            .map_err(|e| ApiError::Network(e.to_string()))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path, header};

    fn make_client(base_url: &str) -> ApiClient {
        ApiClient {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
            base_url: base_url.to_string(),
            api_key:  "test-key".to_string(),
        }
    }

    #[tokio::test]
    async fn test_anonymize_sends_bearer_and_deserializes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/anonymize"))
            .and(header("Authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "anonymized_text": "PERSON_1 lives at STREET_1",
                "session_id": "sess-abc",
                "pii_count": 2,
                "profile": "max",
                "ner_degraded": false,
                "treatment_breakdown": {},
                "kept_entities": []
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let resp = client.anonymize(AnonymizeRequest {
            text: "Jean lives at 12 Rue".to_string(),
            legal_basis: "consent".to_string(),
            ..Default::default()
        }).await.unwrap();

        assert_eq!(resp.session_id, "sess-abc");
        assert_eq!(resp.pii_count, 2);
    }

    #[tokio::test]
    async fn test_delete_document_404_returns_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/v1/documents/missing-id"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "type": "about:blank",
                "title": "Not Found",
                "status": 404,
                "detail": "document not found: missing-id"
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let result = client.delete_document("missing-id").await;
        match result {
            Err(ApiError::Http { status: 404, detail }) => {
                assert!(detail.contains("missing-id"), "detail must name the doc: {detail}");
            }
            other => panic!("expected Http 404, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_list_documents_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/documents"))
            .and(header("Authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "documents": [
                    {"doc_id": "d1", "created_at": 1710000000_i64, "entity_count": 5}
                ]
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let resp = client.list_documents().await.unwrap();
        assert_eq!(resp.documents.len(), 1);
        assert_eq!(resp.documents[0].doc_id, "d1");
    }

    #[tokio::test]
    async fn test_5xx_returns_http_error_variant() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/documents"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "detail": "An internal error occurred"
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let result = client.list_documents().await;
        match result {
            Err(ApiError::Http { status: 500, .. }) => {}
            other => panic!("expected Http 500, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_get_audit_default_limit_in_url() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/audit"))
            .and(wiremock::matchers::query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [],
                "total": 0
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let resp = client.get_audit(50).await.unwrap();
        assert!(resp.events.is_empty());
    }

    #[tokio::test]
    async fn test_review_queue_sends_threshold_and_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/review-queue"))
            .and(wiremock::matchers::query_param("threshold", "20"))
            .and(wiremock::matchers::query_param("limit", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "documents": []
            })))
            .mount(&server).await;

        let client = make_client(&server.uri());
        let resp = client.get_review_queue(20, 50).await.unwrap();
        assert!(resp.documents.is_empty());
    }
}
