use thiserror::Error;

#[derive(Debug, Clone)]
pub struct UsageCap {
    pub monthly_tokens:   u64,
    pub monthly_requests: u32,
}

impl Default for UsageCap {
    fn default() -> Self {
        Self {
            monthly_tokens:   10_000_000,
            monthly_requests: 100_000,
        }
    }
}

#[derive(Debug, Error)]
pub enum BillingError {
    #[error("usage cap exceeded: {0}")]
    CapExceeded(String),
    #[error("billing error: {0}")]
    Internal(#[from] anyhow::Error),
}
