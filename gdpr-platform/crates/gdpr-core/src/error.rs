use thiserror::Error;

#[derive(Debug, Error)]
pub enum GdprError {
    #[error("PII detection failed: {0}")]
    PiiDetection(anyhow::Error),

    #[error("Extraction failed: {0}")]
    Extraction(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Circuit breaker open for service: {service}")]
    CircuitOpen { service: &'static str },

    #[error("Bulkhead full for service: {service}")]
    BulkheadFull { service: &'static str },

    #[error("Document not found: {id}")]
    NotFound { id: String },

    #[error("Audit write failed: {0}")]
    Audit(String),

    #[error("deadpool: {0}")]
    Deadpool(String),
}

pub type Result<T> = std::result::Result<T, GdprError>;

impl From<deadpool_sqlite::PoolError> for GdprError {
    fn from(e: deadpool_sqlite::PoolError) -> Self {
        GdprError::Deadpool(e.to_string())
    }
}

impl From<deadpool_sqlite::CreatePoolError> for GdprError {
    fn from(e: deadpool_sqlite::CreatePoolError) -> Self {
        GdprError::Deadpool(e.to_string())
    }
}
