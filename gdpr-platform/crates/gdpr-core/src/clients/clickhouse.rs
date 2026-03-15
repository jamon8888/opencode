//! ClickHouse audit client (GDPR Art. 30 immutable audit trail).
//!
//! OPT-4: Batched writes via `write_batch` replace per-row POSTs.
//! Writes `GdprAuditRow` records via the ClickHouse HTTP interface.
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
/// OPT-4: Maximum rows per batch write to ClickHouse.
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

/// OPT-4: Session-level row for per-request billing / analytics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprSessionRow {
    pub session_id:         String,
    pub user_id:            String,
    pub request_type:       String,
    pub pii_count:          u32,
    pub processing_time_ms: u32,
    pub ts_unix:            i64,
}

/// OPT-4: Billing row for per-profile usage tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprBillingRow {
    pub billing_id:   String,
    pub user_id:      String,
    pub profile:      String,
    pub doc_count:    u32,
    pub pii_count:    u32,
    pub ts_unix:      i64,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct ClickHouseClient {
    url:     String,
    cb:      Arc<CircuitBreaker>,
    buffer:  Mutex<VecDeque<GdprAuditRow>>,
    http:    reqwest::Client,
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

    /// OPT-4: Record a session-level row.
    pub async fn record_session(&self, row: GdprSessionRow) {
        if let Err(e) = self.write_ndjson("gdpr_sessions", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse session write failed");
        }
    }

    /// OPT-4: Record a profile usage row.
    pub async fn record_profile_usage(&self, row: GdprBillingRow) {
        if let Err(e) = self.write_ndjson("gdpr_billing", &[row]).await {
            tracing::warn!(error = %e, "ClickHouse billing write failed");
        }
    }

    /// OPT-4: Record a key event (e.g. vault rotation, config change).
    pub async fn record_key_event(&self, event: &str, detail: &str) {
        let body = serde_json::json!({ "event": event, "detail": detail,
            "ts_unix": crate::audit::now_unix() }).to_string();
        if let Err(e) = self.post_ndjson("gdpr_key_events", &body).await {
            tracing::warn!(error = %e, "ClickHouse key_event write failed");
        }
    }

    /// OPT-4: Record a billing summary row.
    pub async fn record_billing(&self, row: GdprBillingRow) {
        self.record_profile_usage(row).await;
    }

    /// Number of rows currently in the ring buffer (test helper + metrics).
    pub fn buffer_len(&self) -> usize {
        self.buffer.lock().map(|b| b.len()).unwrap_or(0)
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

    /// OPT-4: Batch write — send multiple rows as NDJSON in a single HTTP POST.
    async fn write_batch(&self, rows: &[GdprAuditRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let body: String = rows.iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        self.post_ndjson("gdpr_audit", &body).await
    }

    /// Generic NDJSON batch writer for any table and serializable row type.
    async fn write_ndjson<T: Serialize>(&self, table: &str, rows: &[T]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let body: String = rows.iter()
            .map(|r| serde_json::to_string(r))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        self.post_ndjson(table, &body).await
    }

    async fn post_ndjson(&self, table: &str, body: &str) -> anyhow::Result<()> {
        let resp = self.http
            .post(format!("{}/", self.url))
            .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
            .header("Content-Type", "application/x-ndjson")
            .body(body.to_string())
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

    /// OPT-4: flush_buffer drains up to BATCH_SIZE rows per write_batch call.
    async fn flush_buffer(&self) {
        loop {
            // Drain a batch under lock, then write without holding the lock
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
                // Re-buffer the batch (push_back, may drop oldest if full)
                for row in batch {
                    self.push_to_buffer(row);
                }
                break; // stop flushing on first failure
            }
            self.cb.record_success();
        }
    }
}
