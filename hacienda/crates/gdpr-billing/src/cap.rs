// crates/gdpr-billing/src/cap.rs

use thiserror::Error;
use crate::pricing::{BillingSnapshot, Plan};

#[derive(Debug, Error)]
pub enum BillingError {
    #[error("usage cap exceeded: {0}")]
    CapExceeded(String),
    #[error("billing error: {0}")]
    Internal(String),
}

pub struct UsageCap {
    pub plan: Plan,
}

impl UsageCap {
    /// Returns Err(CapExceeded) if the current snapshot already meets or exceeds
    /// any monthly plan limit. Called by the meter middleware before forwarding
    /// the request — maps to HTTP 429.
    ///
    /// Enterprise plan always returns Ok(()).
    pub fn check(&self, snapshot: &BillingSnapshot) -> Result<(), BillingError> {
        if self.plan == Plan::Enterprise {
            return Ok(());
        }
        let limits = self.plan.limits();

        if snapshot.total_docs >= limits.monthly_docs {
            return Err(BillingError::CapExceeded(format!(
                "monthly document cap of {} reached ({} used)",
                limits.monthly_docs, snapshot.total_docs
            )));
        }
        if snapshot.total_chars_in >= limits.monthly_chars {
            return Err(BillingError::CapExceeded(format!(
                "monthly character cap of {} reached ({} used)",
                limits.monthly_chars, snapshot.total_chars_in
            )));
        }
        if snapshot.total_rag_queries >= limits.monthly_rag {
            return Err(BillingError::CapExceeded(format!(
                "monthly RAG query cap of {} reached ({} used)",
                limits.monthly_rag, snapshot.total_rag_queries
            )));
        }
        let ai_total = snapshot.total_ai_tokens_in.saturating_add(snapshot.total_ai_tokens_out);
        if ai_total >= limits.monthly_ai_tokens {
            return Err(BillingError::CapExceeded(format!(
                "monthly AI token cap of {} reached ({} used)",
                limits.monthly_ai_tokens, ai_total
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::BillingSnapshot;

    fn snapshot(docs: u64) -> BillingSnapshot {
        BillingSnapshot { tenant_id: "t1".into(), period_ym: 202603, total_docs: docs, ..Default::default() }
    }

    #[test]
    fn test_cap_ok_under_limit() {
        let cap = UsageCap { plan: Plan::Starter };
        assert!(cap.check(&snapshot(999)).is_ok());
    }

    #[test]
    fn test_cap_exceeded_at_limit() {
        let cap = UsageCap { plan: Plan::Starter };
        assert!(cap.check(&snapshot(1_000)).is_err());
    }

    #[test]
    fn test_cap_enterprise_never_exceeded() {
        let cap = UsageCap { plan: Plan::Enterprise };
        let huge = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_docs: u64::MAX, total_chars_in: u64::MAX,
            total_rag_queries: u64::MAX, total_ai_tokens_in: u64::MAX / 2,
            total_ai_tokens_out: u64::MAX / 2,
        };
        assert!(cap.check(&huge).is_ok());
    }

    #[test]
    fn test_cap_error_message_contains_limit() {
        let cap = UsageCap { plan: Plan::Starter };
        let err = cap.check(&snapshot(1_000)).unwrap_err();
        assert!(err.to_string().contains("1000"));
    }
}
