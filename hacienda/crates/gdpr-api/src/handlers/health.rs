use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::AppState;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub db: String,
    pub clickhouse: String,
    pub version: &'static str,
}

// ── GET /health ──────────────────────────────────────────────────────────────

pub async fn get_health(
    State(state): State<AppState>,
) -> ApiResult<Json<HealthResponse>> {
    let mut status = "ok".to_string();

    // Check SQLite pool
    let db = match state.db.get().await {
        Ok(_) => "ok".to_string(),
        Err(e) => {
            tracing::warn!(error = %e, "health: db pool error");
            status = "degraded".to_string();
            "error".to_string()
        }
    };

    // Check ClickHouse
    let clickhouse = if let Some(ref ch) = state.clickhouse {
        match ch
            .http_client()
            .get(format!("{}/?query=SELECT+1", ch.base_url().trim_end_matches('/')))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => "ok".to_string(),
            Ok(resp) => {
                tracing::warn!(status = %resp.status(), "health: clickhouse returned non-200");
                status = "degraded".to_string();
                "error".to_string()
            }
            Err(e) => {
                tracing::warn!(error = %e, "health: clickhouse unreachable");
                status = "degraded".to_string();
                "error".to_string()
            }
        }
    } else {
        "disabled".to_string()
    };

    Ok(Json(HealthResponse {
        status,
        db,
        clickhouse,
        version: env!("CARGO_PKG_VERSION"),
    }))
}

// ── GET /metrics ─────────────────────────────────────────────────────────────

pub async fn get_metrics() -> String {
    "# gdpr-api metrics\n".to_string()
}
