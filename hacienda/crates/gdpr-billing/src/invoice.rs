// crates/gdpr-billing/src/invoice.rs

use serde::{Deserialize, Serialize};
use crate::pricing::{BillingSnapshot, Plan, PriceSheet};

#[derive(Debug, Serialize, Deserialize)]
pub struct Invoice {
    pub tenant_id:  String,
    pub period_ym:  u32,
    pub line_items: Vec<LineItem>,
    pub total_eur:  f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LineItem {
    pub description:    String,
    pub quantity:       u64,
    pub unit_price_eur: f64,
    pub total_eur:      f64,
}

impl Invoice {
    /// Build an invoice from a snapshot, plan, and price sheet.
    /// All arithmetic in millicents; f64 conversion only at the end.
    pub fn from_snapshot(snapshot: &BillingSnapshot, plan: &Plan, sheet: &PriceSheet) -> Self {
        let limits = plan.limits();
        let mut items: Vec<LineItem> = Vec::new();
        let mut total_mc: u64 = 0;

        // Document overage
        let doc_over = snapshot.total_docs.saturating_sub(limits.monthly_docs);
        if doc_over > 0 {
            let cost_mc = ((doc_over as u128) * (sheet.per_doc_millicents as u128))
                .min(u64::MAX as u128) as u64;
            total_mc = total_mc.saturating_add(cost_mc);
            items.push(LineItem {
                description:    format!("Document overage ({} docs)", doc_over),
                quantity:       doc_over,
                unit_price_eur: sheet.per_doc_millicents as f64 / 100_000.0,
                total_eur:      cost_mc as f64 / 100_000.0,
            });
        }

        // Character overage
        let char_over = snapshot.total_chars_in.saturating_sub(limits.monthly_chars);
        if char_over > 0 {
            let cost_mc = (char_over as u128 * sheet.per_char_per_million as u128 / 1_000_000)
                .min(u64::MAX as u128) as u64;
            total_mc = total_mc.saturating_add(cost_mc);
            items.push(LineItem {
                description:    format!("Character overage ({} chars)", char_over),
                quantity:       char_over,
                unit_price_eur: sheet.per_char_per_million as f64 / 100_000.0 / 1_000_000.0,
                total_eur:      cost_mc as f64 / 100_000.0,
            });
        }

        // RAG query overage
        let rag_over = snapshot.total_rag_queries.saturating_sub(limits.monthly_rag);
        if rag_over > 0 {
            let cost_mc = ((rag_over as u128) * (sheet.per_rag_query_millicents as u128))
                .min(u64::MAX as u128) as u64;
            total_mc = total_mc.saturating_add(cost_mc);
            items.push(LineItem {
                description:    format!("RAG search overage ({} queries)", rag_over),
                quantity:       rag_over,
                unit_price_eur: sheet.per_rag_query_millicents as f64 / 100_000.0,
                total_eur:      cost_mc as f64 / 100_000.0,
            });
        }

        // AI token overage (in)
        let half_limit = limits.monthly_ai_tokens / 2;
        let in_over = snapshot.total_ai_tokens_in.saturating_sub(half_limit);
        if in_over > 0 {
            let cost_mc = (in_over as u128 * sheet.per_token_in_per_million as u128 / 1_000_000)
                .min(u64::MAX as u128) as u64;
            total_mc = total_mc.saturating_add(cost_mc);
            items.push(LineItem {
                description:    format!("AI token (input) overage ({} tokens)", in_over),
                quantity:       in_over,
                unit_price_eur: sheet.per_token_in_per_million as f64 / 100_000.0 / 1_000_000.0,
                total_eur:      cost_mc as f64 / 100_000.0,
            });
        }

        // AI token overage (out)
        let out_over = snapshot.total_ai_tokens_out.saturating_sub(half_limit);
        if out_over > 0 {
            let cost_mc = (out_over as u128 * sheet.per_token_out_per_million as u128 / 1_000_000)
                .min(u64::MAX as u128) as u64;
            total_mc = total_mc.saturating_add(cost_mc);
            items.push(LineItem {
                description:    format!("AI token (output) overage ({} tokens)", out_over),
                quantity:       out_over,
                unit_price_eur: sheet.per_token_out_per_million as f64 / 100_000.0 / 1_000_000.0,
                total_eur:      cost_mc as f64 / 100_000.0,
            });
        }

        Invoice {
            tenant_id: snapshot.tenant_id.clone(),
            period_ym: snapshot.period_ym,
            line_items: items,
            total_eur: total_mc as f64 / 100_000.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invoice_no_overage_empty() {
        let snap = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_docs: 100, ..Default::default()
        };
        let inv = Invoice::from_snapshot(&snap, &Plan::Starter, &PriceSheet::default());
        assert!(inv.line_items.is_empty());
        assert_eq!(inv.total_eur, 0.0);
    }

    #[test]
    fn test_invoice_one_doc_over_precision() {
        // 1 doc overage = 8_000 millicents = €0.08000
        let snap = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_docs: 1_001, ..Default::default()
        };
        let inv = Invoice::from_snapshot(&snap, &Plan::Starter, &PriceSheet::default());
        assert_eq!(inv.line_items.len(), 1);
        assert!((inv.total_eur - 0.08).abs() < 1e-10);
    }

    #[test]
    fn test_invoice_millicent_no_float_accumulation() {
        // 154 doc overage × 8_000 mc = 1_232_000 mc = €12.32 exactly
        let snap = BillingSnapshot {
            tenant_id: "t1".into(), period_ym: 202603,
            total_docs: 1_154, ..Default::default()
        };
        let inv = Invoice::from_snapshot(&snap, &Plan::Starter, &PriceSheet::default());
        assert!((inv.total_eur - 12.32).abs() < 1e-10);
    }
}
