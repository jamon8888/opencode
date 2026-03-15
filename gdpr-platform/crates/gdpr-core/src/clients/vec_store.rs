//! In-process vector store backed by SQLite (deadpool-sqlite pool).
//!
//! OPT-3: search() loads rows into local Vec, drops conn, then computes cosine.
//! OPT-6: AVX2/FMA cosine similarity via SIMD intrinsics with scalar fallback.
//!
//! Embeddings are stored as raw f32 BLOBs (little-endian) in the `vec_chunks` table.
//! Activated when `EMBEDDING_URL` is set (any OpenAI-compatible /v1/embeddings endpoint).

use anyhow::{anyhow, Result};
use deadpool_sqlite::Pool;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── Embedding client ──────────────────────────────────────────────────────────

/// Calls an OpenAI-compatible `/v1/embeddings` endpoint.
pub struct EmbeddingClient {
    client: Client,
    /// Base URL, e.g. `http://localhost:11434/v1`
    pub url: String,
    /// Model name forwarded in the request body.
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

// ── Vec hit ───────────────────────────────────────────────────────────────────

/// A search hit returned by [`VecStore::search`].
#[derive(Debug, Serialize, Deserialize)]
pub struct VecHit {
    pub doc_id:     String,
    pub score:      f32,
    /// Full anonymized chunk text (for LLM context).
    pub chunk_text: Option<String>,
    /// 0-based position of this chunk within the document.
    pub chunk_idx:  Option<usize>,
    pub pii_count:  i64,
    pub created_at: i64,
}

// ── Vec store ─────────────────────────────────────────────────────────────────

/// SQLite-backed in-process vector store (deadpool-sqlite pool).
pub struct VecStore {
    pool:      Pool,
    embedding: Option<std::sync::Arc<EmbeddingClient>>,
}

impl VecStore {
    /// Construct from a deadpool Pool (no separate embedding client — activated by env var at search time).
    pub fn new(pool: Pool) -> Self {
        let embedding = std::env::var("EMBEDDING_URL").ok().map(|url| {
            let model = std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "nomic-embed-text".into());
            std::sync::Arc::new(EmbeddingClient::new(url, model))
        });
        Self { pool, embedding }
    }

    /// Construct with an explicit embedding client (tests).
    pub fn with_embedding(pool: Pool, embedding: std::sync::Arc<EmbeddingClient>) -> Self {
        Self { pool, embedding: Some(embedding) }
    }

    /// Embed and insert multiple chunks as SQLite BLOB rows.
    ///
    /// `chunk_tuples`: `(chunk_id UUID, chunk_text, chunk_idx)`.
    pub async fn upsert_chunks(
        &self,
        _doc_id: &str,
        chunk_tuples: &[(String, String, usize)],
    ) -> Result<()> {
        let Some(ref emb) = self.embedding else {
            return Ok(()); // no-op if embedding disabled
        };
        for (chunk_id, chunk_text, _) in chunk_tuples {
            let vector = emb.embed(chunk_text).await?;
            let blob   = f32_to_blob(&vector);
            let chunk_id = chunk_id.clone();
            let conn = self.pool.get().await?;
            conn.interact(move |c| {
                c.execute(
                    "INSERT OR REPLACE INTO vec_chunks (chunk_id, embedding) VALUES (?1, ?2)",
                    rusqlite::params![chunk_id, blob],
                )
            })
            .await
            .map_err(|e| anyhow!("interact error: {e}"))?
            .map_err(|e| anyhow!("upsert_chunks: {e}"))?;
        }
        Ok(())
    }

    /// OPT-3: Embed `query`, load ALL rows into a local Vec (releasing the conn),
    /// then compute cosine similarity in-process without holding the connection.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<VecHit>> {
        let Some(ref emb) = self.embedding else {
            return Err(anyhow!("EMBEDDING_URL not set — vector search disabled"));
        };
        let q_vec = emb.embed(query).await?;

        // OPT-3: load rows into a local Vec, drop conn before cosine computation
        let conn = self.pool.get().await?;
        let rows: Vec<(Vec<u8>, String, String, i64, i64, i64)> = conn.interact(|c| {
            let mut stmt = c.prepare(
                "SELECT vc.embedding,
                        dc.doc_id, dc.chunk_text, dc.chunk_idx,
                        d.pii_count, d.created_at
                 FROM vec_chunks vc
                 JOIN doc_chunks dc ON vc.chunk_id = dc.id
                 JOIN documents  d  ON dc.doc_id   = d.id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,  // embedding blob
                        row.get::<_, String>(1)?,   // doc_id
                        row.get::<_, String>(2)?,   // chunk_text
                        row.get::<_, i64>(3)?,      // chunk_idx
                        row.get::<_, i64>(4)?,      // pii_count
                        row.get::<_, i64>(5)?,      // created_at
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow!("search query: {e}"))?;
        // conn is dropped here — OPT-3 complete

        let mut scored: Vec<(f32, String, String, usize, i64, i64)> = rows
            .into_iter()
            .filter_map(|(blob, doc_id, chunk_text, chunk_idx, pii_count, created_at)| {
                let vec   = blob_to_f32(&blob)?;
                // OPT-6: AVX2 cosine with scalar fallback
                let score = cosine_similarity(&q_vec, &vec);
                Some((score, doc_id, chunk_text, chunk_idx as usize, pii_count, created_at))
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored
            .into_iter()
            .map(|(score, doc_id, chunk_text, chunk_idx, pii_count, created_at)| VecHit {
                doc_id,
                score,
                chunk_text:  Some(chunk_text),
                chunk_idx:   Some(chunk_idx),
                pii_count,
                created_at,
            })
            .collect())
    }

    /// Delete embedding rows by chunk UUID.
    pub async fn delete_by_ids(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids = ids.to_vec();
        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            for id in &ids {
                c.execute("DELETE FROM vec_chunks WHERE chunk_id = ?1", rusqlite::params![id])?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow!("delete_by_ids: {e}"))?;
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn f32_to_blob(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for &f in v {
        b.extend_from_slice(&f.to_le_bytes());
    }
    b
}

fn blob_to_f32(b: &[u8]) -> Option<Vec<f32>> {
    if !b.len().is_multiple_of(4) {
        return None;
    }
    Some(
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

// ── OPT-6: AVX2/FMA cosine similarity ────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn cosine_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let len = a.len();
    let mut dot = _mm256_setzero_ps();
    let mut na  = _mm256_setzero_ps();
    let mut nb  = _mm256_setzero_ps();
    let chunks = len / 8;
    for i in 0..chunks {
        let va = _mm256_loadu_ps(a.as_ptr().add(i * 8));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i * 8));
        dot = _mm256_fmadd_ps(va, vb, dot);
        na  = _mm256_fmadd_ps(va, va, na);
        nb  = _mm256_fmadd_ps(vb, vb, nb);
    }
    // horizontal sum
    let dot_sum = hsum_ps_avx2(dot);
    let na_sum  = hsum_ps_avx2(na);
    let nb_sum  = hsum_ps_avx2(nb);
    // remainder (tail elements not covered by 8-wide SIMD)
    let mut dot_r = 0f32;
    let mut na_r  = 0f32;
    let mut nb_r  = 0f32;
    for i in (chunks * 8)..len {
        dot_r += a[i] * b[i];
        na_r  += a[i] * a[i];
        nb_r  += b[i] * b[i];
    }
    let total_dot = dot_sum + dot_r;
    let total_na  = na_sum + na_r;
    let total_nb  = nb_sum + nb_r;
    if total_na == 0.0 || total_nb == 0.0 { 0.0 }
    else { total_dot / (total_na.sqrt() * total_nb.sqrt()) }
}

#[cfg(target_arch = "x86_64")]
unsafe fn hsum_ps_avx2(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;
    let h    = _mm256_extractf128_ps(v, 1);
    let l    = _mm256_castps256_ps128(v);
    let sum4 = _mm_add_ps(h, l);
    let hi   = _mm_movehl_ps(sum4, sum4);
    let sum2 = _mm_add_ps(sum4, hi);
    let hi2  = _mm_shuffle_ps(sum2, sum2, 0x01);
    _mm_cvtss_f32(_mm_add_ss(sum2, hi2))
}

/// OPT-6: Cosine similarity with AVX2/FMA fast path and scalar fallback.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
        return unsafe { cosine_avx2(a, b) };
    }
    // Scalar fallback
    let dot:    f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
