use serde::{Deserialize, Serialize};
use crate::client::UsageRecord;

#[derive(Debug, Serialize, Deserialize)]
pub struct Invoice {
    pub api_key_id:   String,
    pub month:        String,
    pub line_items:   Vec<LineItem>,
    pub total_eur:    f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LineItem {
    pub description: String,
    pub quantity:    u64,
    pub unit_price:  f64,
    pub total:       f64,
}

impl Invoice {
    pub fn generate(usage: &UsageRecord) -> Self {
        const TOKEN_PRICE_PER_M: f64 = 2.0;  // €2 per million tokens
        let total_tokens = usage.tokens_in + usage.tokens_out;
        let token_cost   = (total_tokens as f64 / 1_000_000.0) * TOKEN_PRICE_PER_M;
        Self {
            api_key_id: usage.api_key_id.clone(),
            month:      usage.month.clone(),
            line_items: vec![
                LineItem {
                    description: format!("Tokens ({} in + {} out)", usage.tokens_in, usage.tokens_out),
                    quantity:    total_tokens,
                    unit_price:  TOKEN_PRICE_PER_M / 1_000_000.0,
                    total:       token_cost,
                },
            ],
            total_eur: token_cost,
        }
    }
}
