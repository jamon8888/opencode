use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use std::collections::HashMap;

use gdpr_core::pii::{AnonProfile, PROFILES};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub description: String,
    pub treatment_count: usize,
}

#[derive(Debug, Serialize)]
pub struct ProfileDetail {
    pub name: String,
    pub description: String,
    pub treatments: HashMap<String, String>,
}

// ── All non-custom profiles ──────────────────────────────────────────────────

const ALL_PROFILES: [AnonProfile; 7] = [
    AnonProfile::Max,
    AnonProfile::Deal,
    AnonProfile::Litige,
    AnonProfile::Contrat,
    AnonProfile::Social,
    AnonProfile::Immo,
    AnonProfile::Restruct,
];

// ── GET /v1/profiles ─────────────────────────────────────────────────────────

pub async fn list_profiles(
    State(_state): State<AppState>,
) -> ApiResult<Json<Vec<ProfileSummary>>> {
    let summaries: Vec<ProfileSummary> = ALL_PROFILES
        .iter()
        .map(|p| ProfileSummary {
            name: p.display_name().to_string(),
            description: p.description().to_string(),
            treatment_count: PROFILES.get(p).map(|m| m.len()).unwrap_or(0),
        })
        .collect();

    Ok(Json(summaries))
}

// ── GET /v1/profiles/:name ───────────────────────────────────────────────────

fn parse_profile(name: &str) -> Option<AnonProfile> {
    // Try serde deserialization (handles lowercase names)
    serde_json::from_str::<AnonProfile>(&format!("\"{}\"", name)).ok()
}

pub async fn get_profile(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Json<ProfileDetail>> {
    let profile = parse_profile(&name)
        .ok_or_else(|| ApiError::UnknownProfile(name.clone()))?;

    if profile == AnonProfile::Custom {
        return Err(ApiError::UnknownProfile(name));
    }

    let treatments = PROFILES
        .get(&profile)
        .map(|m| {
            m.iter()
                .map(|(cat, treat)| {
                    (
                        format!("{:?}", cat).to_lowercase(),
                        format!("{:?}", treat).to_lowercase(),
                    )
                })
                .collect::<HashMap<String, String>>()
        })
        .unwrap_or_default();

    Ok(Json(ProfileDetail {
        name: profile.display_name().to_string(),
        description: profile.description().to_string(),
        treatments,
    }))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_profiles_contains_deal_and_max() {
        let summaries: Vec<ProfileSummary> = ALL_PROFILES
            .iter()
            .map(|p| ProfileSummary {
                name: p.display_name().to_string(),
                description: p.description().to_string(),
                treatment_count: PROFILES.get(p).map(|m| m.len()).unwrap_or(0),
            })
            .collect();

        let names: Vec<&str> = summaries.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"deal"), "profiles must contain 'deal'");
        assert!(names.contains(&"max"), "profiles must contain 'max'");
        assert_eq!(summaries.len(), 7);
    }

    #[test]
    fn test_profile_detail_deal_org_is_pseudonym() {
        let profile = AnonProfile::Deal;
        let treatments = PROFILES
            .get(&profile)
            .map(|m| {
                m.iter()
                    .map(|(cat, treat)| {
                        (
                            format!("{:?}", cat).to_lowercase(),
                            format!("{:?}", treat).to_lowercase(),
                        )
                    })
                    .collect::<HashMap<String, String>>()
            })
            .unwrap_or_default();

        assert_eq!(
            treatments.get("org").map(|s| s.as_str()),
            Some("pseudonym"),
            "deal profile: org should be pseudonym"
        );
    }

    #[test]
    fn test_parse_profile_valid() {
        assert_eq!(parse_profile("deal"), Some(AnonProfile::Deal));
        assert_eq!(parse_profile("max"), Some(AnonProfile::Max));
        assert_eq!(parse_profile("litige"), Some(AnonProfile::Litige));
    }

    #[test]
    fn test_parse_profile_invalid() {
        assert!(parse_profile("nonexistent").is_none());
    }

    #[test]
    fn test_all_profiles_have_nonzero_treatments() {
        for p in &ALL_PROFILES {
            let count = PROFILES.get(p).map(|m| m.len()).unwrap_or(0);
            assert!(count > 0, "profile {:?} should have treatments", p);
        }
    }
}
