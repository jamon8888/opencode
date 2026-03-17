// crates/gdpr-billing/src/meter.rs

use std::sync::Arc;
use serde::{Deserialize, Serialize};
use gdpr_core::clients::ClickHouseClient;

// ── Enums ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    Ingest,
    Search,
    Anonymize,
    AiChat,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NerTier {
    #[default]
    L1,
    L1L2,
}

// ── UsageEvent ────────────────────────────────────────────────────────────────

/// Full row written to the `usage_events` ClickHouse table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub tenant_id:     String,
    pub api_key_id:    String,
    pub request_id:    String,
    pub event_type:    EventType,
    pub document_id:   Option<String>,
    pub chars_in:      u64,
    pub chars_out:     u64,
    pub doc_count:     u32,
    pub chunk_count:   u32,
    pub ai_tokens_in:  u32,
    pub ai_tokens_out: u32,
    pub ner_tier:      NerTier,
    pub latency_ms:    u32,
}

// ── MeteringRecord ────────────────────────────────────────────────────────────

/// Lightweight record inserted into Axum response extensions by each handler.
/// The meter middleware reads this after handler completion to build UsageEvent.
#[derive(Debug, Clone, Default)]
pub struct MeteringRecord {
    pub event_type:    Option<EventType>,
    pub document_id:   Option<String>,
    pub chars_in:      u64,
    pub chars_out:     u64,
    pub doc_count:     u32,
    pub ai_tokens_in:  u32,
    pub ai_tokens_out: u32,
    pub ner_tier:      NerTier,
}

// ── UsageRecord (legacy — retained for client.rs / invoice.rs until Task 5) ──

/// Legacy aggregate used by BillingClient and Invoice::generate.
/// Will be removed in Task 5 when client.rs is deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub api_key_id:      String,
    pub month:           String,
    pub tokens_in:       u64,
    pub tokens_out:      u64,
    pub requests_count:  u32,
}

// ── Meter ─────────────────────────────────────────────────────────────────────

pub struct Meter {
    clickhouse: Arc<ClickHouseClient>,
}

impl Meter {
    pub fn new(clickhouse: Arc<ClickHouseClient>) -> Self {
        Self { clickhouse }
    }

    /// Fire-and-forget: spawns a tokio task, never blocks the caller.
    /// On ClickHouse error: logs via tracing::warn!, does not panic or propagate.
    pub async fn record(&self, event: UsageEvent) {
        let ch = Arc::clone(&self.clickhouse);
        tokio::spawn(async move {
            if let Err(e) = ch.write_batch_table("usage_events", &[event]).await {
                tracing::warn!(error = %e, "Meter::record — ClickHouse write failed");
            }
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metering_record_default() {
        let r = MeteringRecord::default();
        assert_eq!(r.doc_count, 0);
        assert_eq!(r.chars_in, 0);
        assert!(r.event_type.is_none());
    }

    #[test]
    fn test_usage_event_serializes() {
        let event = UsageEvent {
            tenant_id:    "t1".into(),
            api_key_id:   "k1".into(),
            request_id:   "r1".into(),
            event_type:   EventType::Ingest,
            document_id:  Some("d1".into()),
            chars_in:     1000,
            chars_out:    500,
            doc_count:    1,
            chunk_count:  3,
            ai_tokens_in: 0,
            ai_tokens_out: 0,
            ner_tier:     NerTier::L1,
            latency_ms:   42,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event_type\":\"ingest\""));
        assert!(json.contains("\"ner_tier\":\"l1\""));
    }

    #[tokio::test]
    async fn test_meter_record_ch_down_does_not_panic() {
        // Point at a non-existent ClickHouse — record() must complete without panic.
        let ch = Arc::new(ClickHouseClient::new("http://localhost:19999"));
        let meter = Meter::new(ch);
        let event = UsageEvent {
            tenant_id:    "t1".into(),
            api_key_id:   "k1".into(),
            request_id:   "r1".into(),
            event_type:   EventType::Search,
            document_id:  None,
            chars_in:     0,
            chars_out:    0,
            doc_count:    0,
            chunk_count:  0,
            ai_tokens_in: 0,
            ai_tokens_out: 0,
            ner_tier:     NerTier::L1,
            latency_ms:   1,
        };
        meter.record(event).await;
        // Give the spawned task time to complete and log the warning
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // No panic = pass
    }
}
