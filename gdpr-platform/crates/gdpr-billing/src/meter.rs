use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub api_key_id:      String,
    pub month:           String,
    pub tokens_in:       u64,
    pub tokens_out:      u64,
    pub requests_count:  u32,
}
