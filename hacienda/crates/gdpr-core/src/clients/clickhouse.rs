//! ClickHouse audit client (GDPR Art. 30 immutable audit trail).
//!
//! Phase 3: Extended with 4 new tables:
//!   - gdpr_sessions   — session vault tracking
//!   - gdpr_profiles   — profile usage telemetry (SummingMergeTree)
//!   - gdpr_keys       — API key audit trail (ReplacingMergeTree)
//!   - gdpr_billing    — metered usage per API key (SummingMergeTree)
//!
//! Fault tolerance:
//! - Circuit breaker: CB:10 failures / 10s cooldown.
//! - Ring buffer: up to 10 000 rows buffered when CB is open.
//! - Flush: buffer drains via `write_batch` (up to 500 rows per call).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::resilience::CircuitBreaker;

const BUFFER_CAP: usize = 10_000;
/// Maximum rows per batch write to ClickHouse.
const BATCH_SIZE: usize = 500;

// ── Row structs ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprAuditRow {
    pub document_id:          String,
    /// One of: "ingest", "query", "deanonymize", "delete"
    pub action:               String,
    pub pii_count_before:     u32,
    pub pii_count_after:      u32,
    pub ner_degraded:         bool,
    pub processing_time_ms:   u32,
    /// One of: "consent", "contract", "legal_obligation", "vital_interest",
    ///         "public_task", "legitimate_interest"
    pub legal_basis:          String,
    pub user_id:              String,
    pub model_version:        String,
    /// AI Act Art. 9 risk classification: "low" | "medium" | "high" | "critical"
    pub ai_act_risk_level:    String,
    /// AI Act Art. 13 transparency: human-readable summary of what was detected.
    pub decision_explanation: String,
}

/// Session vault tracking — maps to `gdpr_sessions` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprSessionRow {
    pub session_id:  String,
    pub api_key_id:  String,
    pub started_at:  u64,
    pub ended_at:    u64,
    pub token_count: u32,
    pub doc_ids:     Vec<String>,
}

/// Profile usage telemetry — maps to `gdpr_profiles` table.
/// SummingMergeTree accumulates `usage_count` automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprProfileRow {
    pub profile_name:  String,
    pub api_key_id:    String,
    pub usage_count:   u64,
    pub avg_pii_count: f32,
}

/// API key audit trail — maps to `gdpr_keys` table.
/// ReplacingMergeTree deduplicates on (key_id, ts_unix).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprKeyRow {
    pub key_id:     String,
    pub event:      String,
    pub api_key_id: String,
}

/// Metered usage per API key — maps to `gdpr_billing` table.
/// SummingMergeTree accumulates all numeric columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprBillingRow {
    pub api_key_id:      String,
    pub month:           String,
    pub tokens_in:       u64,
    pub tokens_out:      u64,
    pub documents_count: u32,
    pub requests_count:  u32,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct ClickHouseClient {
    url:    String,
    cb:     Arc<CircuitBreaker>,
    buffer: Mutex<VecDeque<GdprAuditRow>>,
    http:   reqwest::Client,
}

impl ClickHouseClient {
    /// Create a new client pointing at `url` (e.g. `"http://localhost:8123"`).
    pub fn new(url: &str) -> Self {
        Self {
            url:    url.to_string(),
            cb:     CircuitBreaker::new("clickhouse", 10, Duration::from_secs(10)),
            buffer: Mutex::new(VecDeque::with_capacity(BUFFER_CAP)),
            http:   reqwest::Client::builder()
                        .timeout(Duration::from_secs(1))
                        .build()
                        .expect("reqwest client"),
        }
    }

    // ── gdpr_audit ────────────────────────────────────────────────────────────

    /// Record a GDPR audit event.
    ///
    /// - If CB is closed: write immediately; on success flush buffered rows.
    /// - If CB is open:   push to ring buffer (drops oldest entry if full).
    /// Never blocks the caller for more than the request timeout (1s).
    pub async fn record(&self, row: GdprAuditRow) {
        if self.cb.is_open() {
            self.push_to_buffer(row);
            return;
        }
        match self.write_batch(&[row.clone()]).await {
            Ok(()) => {
                self.cb.record_success();
                self.flush_buffer().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "ClickHouse write failed — buffering row");
                self.cb.record_failure();
                self.push_to_buffer(row);
            }
        }
    }

    // ── gdpr_sessions ─────────────────────────────────────────────────────────

    /// Record a session-level row into `gdpr_sessions`.
    pub async fn record_session(&self, row: GdprSessionRow) {
        if let Err(e) = self.write_batch_table("gdpr_sessions", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse session write failed");
        }
    }

    // ── gdpr_profiles ─────────────────────────────────────────────────────────

    /// Record a profile usage increment into `gdpr_profiles`.
    /// SummingMergeTree accumulates `usage_count` server-side.
    pub async fn record_profile_usage(&self, row: GdprProfileRow) {
        if let Err(e) = self.write_batch_table("gdpr_profiles", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse profile usage write failed");
        }
    }

    // ── gdpr_keys ─────────────────────────────────────────────────────────────

    /// Record an API key lifecycle event into `gdpr_keys`.
    pub async fn record_key_event(&self, row: GdprKeyRow) {
        if let Err(e) = self.write_batch_table("gdpr_keys", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse key_event write failed");
        }
    }

    // ── gdpr_billing ──────────────────────────────────────────────────────────

    /// Record metered usage into `gdpr_billing`.
    /// SummingMergeTree accumulates numeric columns server-side.
    pub async fn record_billing(&self, row: GdprBillingRow) {
        if let Err(e) = self.write_batch_table("gdpr_billing", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse billing write failed");
        }
    }

    // ── diagnostics ───────────────────────────────────────────────────────────

    /// Number of rows currently in the ring buffer (test helper + metrics).
    pub fn buffer_len(&self) -> usize {
        self.buffer.lock().map(|b| b.len()).unwrap_or(0)
    }

    // ── public generic batch writer ───────────────────────────────────────────

    /// Write a batch of serializable rows to any ClickHouse table as NDJSON.
    ///
    /// ```text
    /// POST /?query=INSERT+INTO+{table}+FORMAT+JSONEachRow
    /// Content-Type: application/x-ndjson
    /// <json1>\n<json2>\n...
    /// ```
    pub async fn write_batch_table(&self, table: &str, rows: &[impl serde::Serialize]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let body: String = rows.iter()
            .map(|r| serde_json::to_string(r).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let resp = self.http
            .post(format!("{}/", self.url))
            .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
            .header("Content-Type", "application/x-ndjson")
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "ClickHouse HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    // ── private ───────────────────────────────────────────────────────────────

    fn push_to_buffer(&self, row: GdprAuditRow) {
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= BUFFER_CAP {
                buf.pop_front(); // ring: drop oldest
                tracing::warn!("ClickHouse ring buffer full — dropped oldest entry");
            }
            buf.push_back(row);
        }
    }

    /// Batch write — send multiple `GdprAuditRow`s as NDJSON in a single HTTP POST.
    async fn write_batch(&self, rows: &[GdprAuditRow]) -> anyhow::Result<()> {
        self.write_batch_table("gdpr_audit", rows).await
    }

    /// Drain up to BATCH_SIZE rows from the ring buffer and flush them.
    async fn flush_buffer(&self) {
        loop {
            let batch: Vec<GdprAuditRow> = {
                let mut buf = match self.buffer.lock() {
                    Ok(b) => b,
                    Err(_) => return,
                };
                if buf.is_empty() { return; }
                let n = buf.len().min(BATCH_SIZE);
                buf.drain(..n).collect()
            };

            if let Err(e) = self.write_batch(&batch).await {
                tracing::warn!(error = %e, count = batch.len(), "ClickHouse flush failed — re-buffering batch");
                self.cb.record_failure();
                for row in batch {
                    self.push_to_buffer(row);
                }
                break; // stop flushing on first failure
            }
            self.cb.record_success();
        }
    }
}
