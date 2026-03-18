use std::sync::Arc;
use gdpr_core::clients::qdrant::QdrantStore;
use std::time::Instant;
use dashmap::DashMap;
use reqwest::Client;
pub use gdpr_billing::{Plan, BillingSnapshot};

#[derive(Clone)]
pub struct AppState {
    // Existing fields — preserved for backward compatibility
    pub db:            deadpool_sqlite::Pool,
    pub keys:          Arc<DashMap<String, ApiKeyRecord>>,
    pub http:          reqwest::Client,
    pub upstream_url:  String,
    pub session_cache: Arc<DashMap<String, SessionCache>>,
    pub engine_pool:   Arc<gdpr_core::pii::pool::EnginePool>,
    pub clickhouse:    Option<Arc<crate::clients::ClickHouseClient>>,

    // New T4 fields
    pub qdrant:              Option<Arc<QdrantStore>>,
    pub http_client:         Client,
    pub key_cache:           Arc<DashMap<String, CachedKey>>,
    pub tensorzero_base_url: String,
    pub tensorzero_key:      String,
    pub jwt_secret:          String,
    pub meter:               std::sync::Arc<gdpr_billing::Meter>,
    pub snapshot_cache:      std::sync::Arc<dashmap::DashMap<String, (BillingSnapshot, Instant)>>,
}

#[derive(Clone, Debug)]
pub struct ApiKeyRecord {
    pub id:       String,
    pub name:     String,
    pub key_hash: String,
}

pub struct SessionCache {
    pub token_map:  DashMap<String, String>,
    pub created_at: Instant,
}

#[derive(Debug, Clone)]
pub struct CachedKey {
    pub tenant_id:  String,
    pub api_key_id: String,
    pub key_hash:   String,
    pub scopes:     Vec<String>,
    pub plan:       Plan,
    pub is_active:  bool,
    pub expires_at: Option<i64>,
    pub cached_at:  i64,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub tenant_id:  String,
    pub api_key_id: String,
    pub scopes:     Vec<String>,
    pub plan:       Plan,
}

impl AuthContext {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }
    pub fn require_scope(&self, scope: &str) -> Result<(), crate::error::ApiError> {
        if self.has_scope(scope) {
            Ok(())
        } else {
            Err(crate::error::ApiError::Forbidden(format!("Missing scope: {}", scope)))
        }
    }
}

