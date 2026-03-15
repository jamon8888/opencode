use std::sync::Arc;
use std::time::Instant;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct AppState {
    pub db:            deadpool_sqlite::Pool,
    pub keys:          Arc<DashMap<String, ApiKeyRecord>>,
    pub http:          reqwest::Client,
    pub upstream_url:  String,
    pub session_cache: Arc<DashMap<String, SessionCache>>,
}

#[derive(Clone, Debug)]
pub struct ApiKeyRecord {
    pub id:       String,
    pub name:     String,
    pub key_hash: String,
}

pub struct SessionCache {
    pub token_map: DashMap<String, String>,
    pub last_used: Instant,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProblemDetail {
    pub r#type:  String,
    pub title:   String,
    pub status:  u16,
    pub detail:  String,
}
