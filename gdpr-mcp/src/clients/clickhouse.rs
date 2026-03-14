//! ClickHouse audit client (GDPR Art. 30 immutable audit trail).
//!
//! Writes `GdprAuditRow` records via the ClickHouse HTTP interface
//! (`INSERT INTO gdpr_audit FORMAT JSONEachRow`).
//!
//! Fault tolerance:
//! - Circuit breaker: CB:10 failures / 10s cooldown.
//! - Ring buffer: up to 10 000 rows buffered when CB is open.
//! - Flush: buffer drains on the next successful write.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::resilience::CircuitBreaker;

const BUFFER_CAP: usize = 10_000;

// ── Row struct ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprAuditRow {
    pub document_id:        String,
    /// One of: "ingest", "query", "deanonymize", "delete"
    pub action:             String,
    pub pii_count_before:   u32,
    pub pii_count_after:    u32,
    pub ner_degraded:       bool,
    pub processing_time_ms: u32,
    /// One of: "consent", "contract", "legal_obligation", "vital_interest",
    ///         "public_task", "legitimate_interest"
    pub legal_basis:        String,
    pub user_id:            String,
    pub model_version:      String,
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
    pub fn new(url: &str) -> Arc<Self> {
        Arc::new(Self {
            url:    url.to_string(),
            cb:     CircuitBreaker::new("clickhouse", 10, Duration::from_secs(10)),
            buffer: Mutex::new(VecDeque::with_capacity(BUFFER_CAP)),
            http:   reqwest::Client::builder()
                        .timeout(Duration::from_secs(1))
                        .build()
                        .expect("reqwest client"),
        })
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
        match self.write_one(&row).await {
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

    async fn write_one(&self, row: &GdprAuditRow) -> anyhow::Result<()> {
        let body = serde_json::to_string(row)?;
        let resp = self.http
            .post(format!("{}/", self.url))
            .query(&[("query", "INSERT INTO gdpr_audit FORMAT JSONEachRow")])
            .header("Content-Type", "application/json")
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

    async fn flush_buffer(&self) {
        // Drain under lock, then write without holding the lock
        let rows: Vec<GdprAuditRow> = {
            let mut buf = match self.buffer.lock() {
                Ok(b) => b,
                Err(_) => return,
            };
            buf.drain(..).collect()
        };

        for row in rows {
            if let Err(e) = self.write_one(&row).await {
                tracing::warn!(error = %e, "ClickHouse flush failed — re-buffering");
                self.cb.record_failure();
                self.push_to_buffer(row);
                break; // stop flushing on first failure
            }
            self.cb.record_success();
        }
    }
}
