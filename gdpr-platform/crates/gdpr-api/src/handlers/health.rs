use axum::Json;
use serde_json::{json, Value};

pub async fn get_health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "gdpr-api"}))
}

pub async fn get_metrics() -> String {
    "# gdpr-api metrics\n".to_string()
}
