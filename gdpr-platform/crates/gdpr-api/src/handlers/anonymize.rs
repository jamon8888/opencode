use std::collections::HashMap;
use std::time::Instant;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use validator::Validate;

use gdpr_core::pii::{
    AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool,
};

use crate::clients::GdprAuditRow;
use crate::error::{ApiError, ApiResult};
use crate::extractors::ValidJson;
use crate::state::AppState;

// ── DetectionMode ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetectionMode {
    Fast,
    #[default]
    Balanced,
    Strict,
}

// ── Request / Response ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Validate)]
pub struct AnonymizeRequest {
    #[validate(length(min = 1, max = 100_000))]
    pub text: String,
    #[serde(default)]
    pub profile: AnonProfile,
    #[serde(default)]
    pub mode: DetectionMode,
    pub session_id: Option<String>,
    #[validate(length(min = 1))]
    pub legal_basis: String,
}

#[derive(Debug, Serialize)]
pub struct AnonymizeResponse {
    pub anonymized_text: String,
    pub profile: AnonProfile,
    pub session_id: String,
    pub pii_count: usize,
    pub ner_degraded: bool,
    pub treatment_breakdown: HashMap<String, String>,
    pub kept_entities: Vec<String>,
}

// ── POST /v1/anonymize ───────────────────────────────────────────────────────

pub async fn post_anonymize(
    State(state): State<AppState>,
    ValidJson(req): ValidJson<AnonymizeRequest>,
) -> ApiResult<Json<AnonymizeResponse>> {
    let start = Instant::now();
    let session_id = req.session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let profile = req.profile;
    let legal_basis = req.legal_basis.clone();
    let text = req.text;

    // Create session context and treatment engine
    let mut session_ctx = SessionContext::new(profile);
    let pool_strings: Vec<String> = get_pool(&profile).iter().map(|s| s.to_string()).collect();
    let engine = TreatmentEngine::new(pool_strings);

    // Run CPU-bound anonymization on blocking thread
    let result = tokio::task::spawn_blocking(move || {
        anonymize_with_profile(&text, profile, &mut session_ctx, &engine)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("spawn_blocking join error: {e}")))?
    .map_err(|e| ApiError::Internal(format!("anonymize_with_profile failed: {e}")))?;

    let processing_ms = start.elapsed().as_millis() as u32;

    // Store session in cache for future deanonymize lookups
    // TODO: T7 — populate session_cache token_map from SessionContext

    // Emit ClickHouse audit row (best-effort)
    if let Some(ref ch) = state.clickhouse {
        let row = GdprAuditRow {
            document_id:         session_id.clone(),
            action:              "anonymize".to_string(),
            pii_count_before:    result.pii_count as u32,
            pii_count_after:     0,
            ner_degraded:        if result.ner_degraded { 1 } else { 0 },
            processing_time_ms:  processing_ms,
            legal_basis:         legal_basis,
            user_id:             String::new(), // TODO: T7 — extract from AuthContext
            model_version:       env!("CARGO_PKG_VERSION").to_string(),
            ai_act_risk_level:   "low".to_string(),
            decision_explanation: format!("profile={}", profile.display_name()),
        };
        ch.write_audit_row(row).await;
    }

    Ok(Json(AnonymizeResponse {
        anonymized_text: result.text,
        profile: result.profile,
        session_id,
        pii_count: result.pii_count,
        ner_degraded: result.ner_degraded,
        treatment_breakdown: result.treatment_breakdown,
        kept_entities: result.kept_entities,
    }))
}

// ── Deanonymize ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DeanonymizeReq {
    pub text: String,
    pub session_id: Option<String>,
}

#[derive(Serialize)]
pub struct DeanonymizeResp {
    pub text: String,
}

pub async fn post_deanonymize(
    State(state): State<AppState>,
    Json(req): Json<DeanonymizeReq>,
) -> ApiResult<Json<DeanonymizeResp>> {
    let result_text = if let Some(ref sid) = req.session_id {
        if let Some(session) = state.session_cache.get(sid) {
            // Build reverse map: token -> original
            let mut reversed = req.text.clone();
            for entry in session.token_map.iter() {
                let original = entry.key();
                let token = entry.value();
                reversed = reversed.replace(token.as_str(), original);
            }
            reversed
        } else {
            // Session not found — return text unchanged
            req.text
        }
    } else {
        req.text
    };

    Ok(Json(DeanonymizeResp { text: result_text }))
}
