//! ClickHouse GDPR Art. 30 audit trail client.
//!
//! Design:
//! - INSERT via HTTP `POST ?query=INSERT%20INTO%20gdpr.gdpr_audit%20FORMAT%20JSONEachRow` + JSON body.
//! - Circuit breaker: opens after 10 consecutive failures within 10 s; resets on success.
//! - Ring buffer: when CB open, rows are buffered (up to 10 000, oldest dropped on overflow).
//!   On successful write, buffered rows are flushed in a single batch INSERT.
//! - All methods are best-effort: errors are logged, never propagated to the caller.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const RING_BUFFER_CAP: usize = 10_000;
const CB_FAILURE_THRESHOLD: u32 = 10;
const CB_WINDOW_SECS: u64 = 10;

/// A single row for the `gdpr_audit` ClickHouse table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprAuditRow {
    pub document_id:         String,
    pub action:              String,   // "ingest" | "query" | "deanonymize" | "delete"
    pub pii_count_before:    u32,
    pub pii_count_after:     u32,
    pub ner_degraded:        u8,       // 0 = L1+L2, 1 = L1 only
    pub processing_time_ms:  u32,
    pub legal_basis:         String,   // GDPR Art. 6 basis, e.g. "contract"
    pub user_id:             String,
    pub model_version:       String,
    pub ai_act_risk_level:   String,   // "low" | "medium" | "high" | "critical"
    pub decision_explanation: String,
}

pub struct ClickHouseClient {
    http:     reqwest::Client,
    base_url: String,   // e.g. "http://clickhouse:8123"
    // Circuit breaker state
    failures:    AtomicU32,
    window_start: AtomicU64,  // unix secs when current failure window started
    // Ring buffer for rows buffered when CB is open
    buffer: Mutex<VecDeque<GdprAuditRow>>,
}

impl ClickHouseClient {
    pub fn new(base_url: impl Into<String>, http: reqwest::Client) -> Arc<Self> {
        Arc::new(Self {
            http,
            base_url: base_url.into(),
            failures: AtomicU32::new(0),
            window_start: AtomicU64::new(0),
            buffer: Mutex::new(VecDeque::new()),
        })
    }

    /// Write a single audit row. Best-effort — never fails the caller.
    ///
    /// If the circuit breaker is open, the row is buffered. On successful
    /// write, any buffered rows are flushed in a single subsequent batch.
    pub async fn write_audit_row(&self, row: GdprAuditRow) {
        if self.is_open() {
            self.push_buffer(row);
            return;
        }
        match self.insert_rows(&[&row]).await {
            Ok(()) => {
                self.record_success();
                self.flush_buffer().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "ClickHouse write failed — buffering row");
                self.record_failure();
                self.push_buffer(row);
            }
        }
    }

    // ── Accessors for external query use ────────────────────────────────────

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    // ── Circuit breaker ──────────────────────────────────────────────────────

    fn is_open(&self) -> bool {
        let now = now_secs();
        let win = self.window_start.load(Ordering::Relaxed);
        // Reset window if expired
        if now.saturating_sub(win) > CB_WINDOW_SECS {
            return false;
        }
        self.failures.load(Ordering::Relaxed) >= CB_FAILURE_THRESHOLD
    }

    fn record_failure(&self) {
        let now = now_secs();
        let win = self.window_start.load(Ordering::Relaxed);
        if now.saturating_sub(win) > CB_WINDOW_SECS {
            // Start a new window
            self.window_start.store(now, Ordering::Relaxed);
            self.failures.store(1, Ordering::Relaxed);
        } else {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_success(&self) {
        self.failures.store(0, Ordering::Relaxed);
        self.window_start.store(0, Ordering::Relaxed);
    }

    // ── Ring buffer ──────────────────────────────────────────────────────────

    fn push_buffer(&self, row: GdprAuditRow) {
        if let Ok(mut buf) = self.buffer.lock() {
            if buf.len() >= RING_BUFFER_CAP {
                buf.pop_front(); // drop oldest
            }
            buf.push_back(row);
        }
    }

    async fn flush_buffer(&self) {
        let rows: Vec<GdprAuditRow> = {
            let Ok(mut buf) = self.buffer.lock() else { return; };
            if buf.is_empty() { return; }
            buf.drain(..).collect()
        };
        let refs: Vec<&GdprAuditRow> = rows.iter().collect();
        match self.insert_rows(&refs).await {
            Ok(()) => tracing::info!(count = rows.len(), "ClickHouse buffer flushed"),
            Err(e) => {
                tracing::warn!(error = %e, count = rows.len(), "ClickHouse buffer flush failed — re-buffering");
                self.record_failure();
                if let Ok(mut buf) = self.buffer.lock() {
                    // Re-insert in reverse order at the front so oldest rows are evicted last
                    for row in rows.into_iter().rev() {
                        if buf.len() >= RING_BUFFER_CAP {
                            buf.pop_back(); // drop newest to protect re-buffered old events
                        }
                        buf.push_front(row);
                    }
                }
            }
        }
    }

    // ── HTTP insert ──────────────────────────────────────────────────────────

    async fn insert_rows(&self, rows: &[&GdprAuditRow]) -> anyhow::Result<()> {
        let body = rows
            .iter()
            .map(|r| serde_json::to_string(r))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");

        let url = format!(
            "{}?query=INSERT%20INTO%20gdpr.gdpr_audit%20FORMAT%20JSONEachRow",
            self.base_url.trim_end_matches('/')
        );

        let resp = self.http
            .post(&url)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let raw    = resp.text().await.unwrap_or_default();
            let body   = raw.chars().take(200).collect::<String>();
            anyhow::bail!("ClickHouse INSERT failed: {} — {}", status, body);
        }
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
