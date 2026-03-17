// crates/gdpr-billing/src/pricing.rs

use serde::{Deserialize, Serialize};

// ── Plan ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    Starter,
    Business,
    Enterprise,
}

pub struct PlanLimits {
    pub monthly_docs:      u64,
    pub monthly_chars:     u64,
    pub monthly_rag:       u64,
    pub monthly_ai_tokens: u64,  // in + out combined
}

impl Plan {
    pub fn limits(&self) -> PlanLimits {
        match self {
            Plan::Starter => PlanLimits {
                monthly_docs:      1_000,
                monthly_chars:     5_000_000,
                monthly_rag:       500,
                monthly_ai_tokens: 500_000,
            },
            Plan::Business => PlanLimits {
                monthly_docs:      10_000,
                monthly_chars:     50_000_000,
                monthly_rag:       5_000,
                monthly_ai_tokens: 5_000_000,
            },
            Plan::Enterprise => PlanLimits {
                monthly_docs:      u64::MAX,
                monthly_chars:     u64::MAX,
                monthly_rag:       u64::MAX,
                monthly_ai_tokens: u64::MAX,
            },
        }
    }

    pub fn rate_limit_rpm(&self) -> u32 {
        match self {
            Plan::Starter    => 60,
            Plan::Business   => 300,
            Plan::Enterprise => 1000,
        }
    }

    pub fn max_concurrent(&self) -> usize {
        match self {
            Plan::Starter    => 5,
            Plan::Business   => 20,
            Plan::Enterprise => 100,
        }
    }
}

// ── PriceSheet ────────────────────────────────────────────────────────────────

/// All values in EUR millicents (1 millicent = €0.00001).
pub struct PriceSheet {
    pub per_doc_millicents:        u64,  // 8_000   = €0.08
    pub per_char_per_million:      u64,  // 40_000  = €0.40/1M chars
    pub per_rag_query_millicents:  u64,  // 4_000   = €0.04
    pub per_token_in_per_million:  u64,  // 150_000 = €1.50/1M tokens
    pub per_token_out_per_million: u64,  // 200_000 = €2.00/1M tokens
    pub l2_surcharge_per_doc:      u64,  // 2_000   = €0.02
}

impl Default for PriceSheet {
    fn default() -> Self {
        Self {
            per_doc_millicents:        8_000,
            per_char_per_million:      40_000,
            per_rag_query_millicents:  4_000,
            per_token_in_per_million:  150_000,
            per_token_out_per_million: 200_000,
            l2_surcharge_per_doc:      2_000,
        }
    }
}

impl PriceSheet {
    /// Returns total overage charge in EUR millicents.
    /// Enterprise plan always returns 0. Pure function — no I/O.
    pub fn calculate_overage(&self, snapshot: &BillingSnapshot, plan: &Plan) -> u64 {
        if *plan == Plan::Enterprise {
            return 0;
        }
        let limits = plan.limits();

        // Document overage
        let doc_over = snapshot.total_docs.saturating_sub(limits.monthly_docs);
        let doc_cost = ((doc_over as u128) * (self.per_doc_millicents as u128))
            .min(u64::MAX as u128) as u64;

        // Character overage (pro-rated per million)
        let char_over = snapshot.total_chars_in.saturating_sub(limits.monthly_chars);
        let char_cost = millicents_per_million(char_over, self.per_char_per_million);

        // RAG query overage
        let rag_over = snapshot.total_rag_queries.saturating_sub(limits.monthly_rag);
        let rag_cost = ((rag_over as u128) * (self.per_rag_query_millicents as u128))
            .min(u64::MAX as u128) as u64;

        // AI token overage — limit is shared; split evenly between in/out
        let half_limit = limits.monthly_ai_tokens / 2;
        let in_over  = snapshot.total_ai_tokens_in.saturating_sub(half_limit);
        let out_over = snapshot.total_ai_tokens_out.saturating_sub(half_limit);
        let token_cost = millicents_per_million(in_over, self.per_token_in_per_million)
                       + millicents_per_million(out_over, self.per_token_out_per_million);

        doc_cost
            .saturating_add(char_cost)
            .saturating_add(rag_cost)
            .saturating_add(token_cost)
    }
}

/// Compute `(quantity / 1_000_000) * rate_per_million` without overflow.
/// Uses u128 for the intermediate multiply then clamps to u64::MAX.
fn millicents_per_million(quantity: u64, rate_per_million: u64) -> u64 {
    let cost = (quantity as u128) * (rate_per_million as u128) / 1_000_000;
    cost.min(u64::MAX as u128) as u64
}

// ── BillingSnapshot ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillingSnapshot {
    pub tenant_id:           String,  // Default = ""
    pub period_ym:           u32,     // YYYYMM e.g. 202603; Default = 0
    pub total_docs:          u64,
    pub total_chars_in:      u64,
    pub total_rag_queries:   u64,
    pub total_ai_tokens_in:  u64,
    pub total_ai_tokens_out: u64,
}

impl BillingSnapshot {
    /// Current period as YYYYMM u32.
    pub fn current_period_ym() -> u32 {
        chrono::Utc::now()
            .format("%Y%m")
            .to_string()
            .parse::<u32>()
            .unwrap_or(0)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn starter_snapshot(docs: u64) -> BillingSnapshot {
        BillingSnapshot { tenant_id: "t1".into(), period_ym: 202603, total_docs: docs, ..Default::default() }
    }

    #[test]
    fn test_overage_starter_under_limit() {
        assert_eq!(PriceSheet::default().calculate_overage(&starter_snapshot(999), &Plan::Starter), 0);
    }

    #[test]
    fn test_overage_starter_exactly_at_limit() {
        assert_eq!(PriceSheet::default().calculate_overage(&starter_snapshot(1_000), &Plan::Starter), 0);
    }

    #[test]
    fn test_overage_starter_one_doc_over() {
        // 1 doc over → 1 × 8_000 millicents
        assert_eq!(PriceSheet::default().calculate_overage(&starter_snapshot(1_001), &Plan::Starter), 8_000);
    }

    #[test]
    fn test_overage_enterprise_always_zero() {
        let huge = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_docs: 1_000_000, total_chars_in: 1_000_000_000,
            total_rag_queries: 1_000_000, total_ai_tokens_in: 1_000_000_000,
            total_ai_tokens_out: 1_000_000_000,
        };
        assert_eq!(PriceSheet::default().calculate_overage(&huge, &Plan::Enterprise), 0);
    }

    #[test]
    fn test_millicents_per_million_no_overflow() {
        // 2^63 quantity × large rate should not panic
        let _ = millicents_per_million(u64::MAX, 200_000);
    }

    #[test]
    fn test_plan_rate_limits() {
        assert_eq!(Plan::Starter.rate_limit_rpm(), 60);
        assert_eq!(Plan::Business.rate_limit_rpm(), 300);
        assert_eq!(Plan::Enterprise.rate_limit_rpm(), 1000);
    }

    #[test]
    fn test_overage_char_cost() {
        // 1_000_000 chars over limit → millicents_per_million(1_000_000, 40_000)
        // = (1_000_000 * 40_000) / 1_000_000 = 40_000
        let snap = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_chars_in: 6_000_000,  // Starter limit 5_000_000 → 1_000_000 over
            ..Default::default()
        };
        assert_eq!(PriceSheet::default().calculate_overage(&snap, &Plan::Starter), 40_000);
    }

    #[test]
    fn test_overage_token_cost() {
        // Starter monthly_ai_tokens = 500_000; half_limit = 250_000
        // 300_000 tokens_in → in_over = 50_000; cost = millicents_per_million(50_000, 150_000)
        // = (50_000 * 150_000) / 1_000_000 = 7_500_000_000 / 1_000_000 = 7_500
        let snap = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_ai_tokens_in: 300_000,
            ..Default::default()
        };
        assert_eq!(PriceSheet::default().calculate_overage(&snap, &Plan::Starter), 7_500);
    }
}
