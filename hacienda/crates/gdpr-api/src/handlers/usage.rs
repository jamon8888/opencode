// crates/gdpr-api/src/handlers/usage.rs

use axum::{extract::State, Extension, Json};
use gdpr_billing::{BillingSnapshot, Invoice, Plan, PriceSheet};
use crate::state::{AppState, AuthContext};
use crate::error::ApiResult;

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Invoice>> {
    let period_ym = BillingSnapshot::current_period_ym();

    // Try snapshot cache first (populated by meter middleware)
    let snapshot = if let Some(entry) = state.snapshot_cache.get(&auth.tenant_id) {
        entry.0.clone()
    } else if let Some(ch) = &state.clickhouse {
        crate::query_billing_snapshot(ch, &auth.tenant_id, period_ym)
            .await
            .unwrap_or_else(|_| BillingSnapshot {
                tenant_id: auth.tenant_id.clone(),
                period_ym,
                ..Default::default()
            })
    } else {
        BillingSnapshot {
            tenant_id: auth.tenant_id.clone(),
            period_ym,
            ..Default::default()
        }
    };

    let invoice = Invoice::from_snapshot(&snapshot, &auth.plan, &PriceSheet::default());
    Ok(Json(invoice))
}
