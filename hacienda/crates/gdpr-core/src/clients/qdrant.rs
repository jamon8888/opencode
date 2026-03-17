//! Qdrant vector store client + embedding client.
//!
//! Uses Qdrant's REST API via reqwest (consistent with the ClickHouse client pattern).
//! Embedding is delegated to any OpenAI-compatible `/v1/embeddings` endpoint.
//!
//! Activated when `QDRANT_URL` is set.

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

// ── Embedding client ──────────────────────────────────────────────────────────

/// Calls an OpenAI-compatible `/v1/embeddings` endpoint.
pub struct EmbeddingClient {
    client: Client,
    url: String,
    pub model: String,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

impl EmbeddingClient {
    pub fn new(url: String, model: String) -> Self {
        Self { client: Client::new(), url, model }
    }

    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let resp: EmbeddingResponse = self
            .client
            .post(format!("{}/embeddings", self.url))
            .json(&json!({ "model": self.model, "input": text }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        resp.data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| anyhow!("Empty embedding response"))
    }
}

// ── Qdrant store ──────────────────────────────────────────────────────────────

/// A Qdrant search hit returned by [`QdrantStore::search`].
#[derive(Debug, Serialize, Deserialize)]
pub struct QdrantHit {
    pub doc_id:     String,
    pub score:      f32,
    pub chunk_text: Option<String>,
    pub chunk_idx:  Option<usize>,
}

/// Thin async wrapper over the Qdrant REST API.
pub struct QdrantStore {
    client: Client,
    url: String,
    pub collection: String,
    pub embedding: Option<Arc<EmbeddingClient>>,
    pub dim: u64,
}

impl QdrantStore {
    /// Build from environment variables.
    pub fn from_env() -> Option<Arc<Self>> {
        let url = std::env::var("QDRANT_URL").ok()?;
        let collection =
            std::env::var("QDRANT_COLLECTION").unwrap_or_else(|_| "gdpr_docs".into());
        let dim: u64 = std::env::var("EMBEDDING_DIM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(384);

        let embedding = std::env::var("EMBEDDING_URL").ok().map(|emb_url| {
            let model = std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "BAAI/bge-small-en-v1.5".into());
            tracing::info!(url = %emb_url, model = %model, "Embedding client enabled");
            Arc::new(EmbeddingClient::new(emb_url, model))
        });

        tracing::info!(url = %url, collection = %collection, dim, "Qdrant store enabled");
        Some(Arc::new(Self {
            client: Client::new(),
            url,
            collection,
            embedding,
            dim,
        }))
    }

    /// Create the collection if it does not already exist.
    pub async fn ensure_collection(&self) -> Result<()> {
        let exists = self
            .client
            .get(format!("{}/collections/{}", self.url, self.collection))
            .send()
            .await?
            .status()
            .is_success();

        if exists {
            return Ok(());
        }

        self.client
            .put(format!("{}/collections/{}", self.url, self.collection))
            .json(&json!({
                "vectors": { "size": self.dim, "distance": "Cosine" }
            }))
            .send()
            .await?
            .error_for_status()?;

        tracing::info!(collection = %self.collection, dim = self.dim, "Qdrant collection created");
        Ok(())
    }

    /// Embed `text` and upsert the vector with `doc_id` as the point ID.
    pub async fn upsert(&self, doc_id: &str, text: &str) -> Result<()> {
        let Some(ref emb) = self.embedding else {
            tracing::warn!(doc_id, "EMBEDDING_URL not set — skipping Qdrant upsert");
            return Ok(());
        };

        let vector = emb.embed(text).await?;

        self.client
            .put(format!("{}/collections/{}/points", self.url, self.collection))
            .json(&json!({
                "points": [{
                    "id": doc_id,
                    "vector": vector,
                    "payload": { "doc_id": doc_id }
                }]
            }))
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Embed `query` and return the top-`limit` nearest document IDs.
    pub async fn search(&self, query: &str, limit: u64) -> Result<Vec<QdrantHit>> {
        let Some(ref emb) = self.embedding else {
            return Err(anyhow!("EMBEDDING_URL not set — cannot perform vector search"));
        };

        let vector = emb.embed(query).await?;

        let resp: Value = self
            .client
            .post(format!("{}/collections/{}/points/search", self.url, self.collection))
            .json(&json!({
                "vector": vector,
                "limit": limit,
                "with_payload": true
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let hits = resp["result"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let doc_id     = r["payload"]["doc_id"].as_str()?.to_string();
                let score      = r["score"].as_f64()? as f32;
                let chunk_text = r["payload"]["chunk_text"].as_str().map(|s| s.to_string());
                let chunk_idx  = r["payload"]["chunk_idx"].as_u64().map(|v| v as usize);
                Some(QdrantHit { doc_id, score, chunk_text, chunk_idx })
            })
            .collect();

        Ok(hits)
    }

    /// Remove a point from the collection by `doc_id`.
    pub async fn delete(&self, doc_id: &str) -> Result<()> {
        self.client
            .post(format!("{}/collections/{}/points/delete", self.url, self.collection))
            .json(&json!({ "points": [doc_id] }))
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Embed and upsert multiple chunks for one document as separate Qdrant points.
    pub async fn upsert_chunks(
        &self,
        doc_id: &str,
        chunk_tuples: &[(String, String, usize)],
    ) -> Result<()> {
        let Some(ref emb) = self.embedding else {
            tracing::warn!(doc_id, "EMBEDDING_URL not set — skipping Qdrant chunk upsert");
            return Ok(());
        };

        let mut points = Vec::with_capacity(chunk_tuples.len());
        for (chunk_id, chunk_text, chunk_idx) in chunk_tuples {
            let vector = emb.embed(chunk_text).await?;
            points.push(json!({
                "id": chunk_id,
                "vector": vector,
                "payload": {
                    "doc_id":     doc_id,
                    "chunk_idx":  chunk_idx,
                    "chunk_text": chunk_text
                }
            }));
        }

        if points.is_empty() {
            return Ok(());
        }

        self.client
            .put(format!("{}/collections/{}/points", self.url, self.collection))
            .json(&json!({ "points": points }))
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    /// Delete Qdrant points by explicit point IDs (chunk UUIDs).
    pub async fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        self.client
            .post(format!("{}/collections/{}/points/delete", self.url, self.collection))
            .json(&json!({ "points": ids }))
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }
}
