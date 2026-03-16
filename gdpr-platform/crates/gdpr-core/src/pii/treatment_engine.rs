//! Treatment engine with session-aware token management.
//!
//! Each anonymization session gets a `SessionContext` that tracks counters,
//! value-to-token mappings, and pseudonym assignments for stable deduplication
//! within a single session.

use std::collections::HashMap;
use chrono::NaiveDate;
use regex::Regex;
use once_cell::sync::Lazy;

use super::entity_category::EntityCategory;
use super::treatment::Treatment;
use super::profile::AnonProfile;
use super::profile_registry::get_treatment;
use super::deduplication::deduplicate;
use super::legal_patterns_fr::detect_legal_patterns_fr;

// ── SessionContext ──────────────────────────────────────────────────────────

/// Per-session state for stable token assignment.
///
/// Each new `session_id` gets a fresh `SessionContext::new()`.
/// Do NOT share contexts between sessions.
pub struct SessionContext {
    pub profile: AnonProfile,
    counters: HashMap<String, u32>,
    value_to_token: HashMap<String, String>,
    pseudonym_index: HashMap<String, usize>,
    pub date_anchor: Option<NaiveDate>,
}

impl SessionContext {
    pub fn new(profile: AnonProfile) -> Self {
        Self {
            profile,
            counters: HashMap::new(),
            value_to_token: HashMap::new(),
            pseudonym_index: HashMap::new(),
            date_anchor: None,
        }
    }
}

// ── TreatmentEngine ─────────────────────────────────────────────────────────

/// Applies treatments (mask, pseudonym, generalize, relativize, keep) to detected PII.
pub struct TreatmentEngine {
    pool: Vec<String>,
}

impl TreatmentEngine {
    pub fn new(pool: Vec<String>) -> Self {
        Self { pool }
    }

    /// Apply the given treatment to a detected PII value.
    pub fn apply(
        &self,
        value: &str,
        entity_type: &str,
        category: EntityCategory,
        treatment: Treatment,
        ctx: &mut SessionContext,
    ) -> String {
        match treatment {
            Treatment::Mask       => self.mask(value, entity_type, category, ctx),
            Treatment::Pseudonym  => self.pseudonym(value, category, ctx),
            Treatment::Generalize => self.generalize(value, category),
            Treatment::Relativize => self.relativize(value, entity_type, category, ctx),
            Treatment::Keep       => value.to_string(),
        }
    }

    // ── Mask ─────────────────────────────────────────────────────────────

    fn mask(
        &self,
        value: &str,
        entity_type: &str,
        category: EntityCategory,
        ctx: &mut SessionContext,
    ) -> String {
        if let Some(existing) = ctx.value_to_token.get(value) {
            return existing.clone();
        }
        let counter = ctx.counters.entry(entity_type.to_string()).or_insert(0);
        *counter += 1;
        let token = format!("[{}_{counter}]", category.placeholder_base());
        ctx.value_to_token.insert(value.to_string(), token.clone());
        token
    }

    // ── Pseudonym ────────────────────────────────────────────────────────

    fn pseudonym(
        &self,
        value: &str,
        category: EntityCategory,
        ctx: &mut SessionContext,
    ) -> String {
        if let Some(existing) = ctx.value_to_token.get(value) {
            return existing.clone();
        }
        if self.pool.is_empty() {
            // Fallback to mask when no pseudonym pool available
            let entity_type = category.placeholder_base();
            return self.mask(value, entity_type, category, ctx);
        }
        // Use sequential index (number of unique pseudonymized values so far).
        // When pool is exhausted, fall back to mask — avoids collision where
        // two distinct values would receive the same pseudonym via modular wrap.
        let index = ctx.pseudonym_index.len();
        if index >= self.pool.len() {
            // Pool exhausted: fall back to mask to guarantee uniqueness
            let entity_type = category.placeholder_base();
            return self.mask(value, entity_type, category, ctx);
        }
        let pseudonym = self.pool[index].clone();
        ctx.pseudonym_index.insert(value.to_string(), index);
        ctx.value_to_token.insert(value.to_string(), pseudonym.clone());
        pseudonym
    }

    // ── Generalize ───────────────────────────────────────────────────────

    fn generalize(&self, value: &str, category: EntityCategory) -> String {
        match category {
            EntityCategory::Amount | EntityCategory::AmountRange => {
                self.generalize_amount(value)
            }
            EntityCategory::Addr => {
                self.generalize_addr(value)
            }
            _ => "[GENERALIZED]".to_string(),
        }
    }

    fn generalize_amount(&self, value: &str) -> String {
        // Extract numeric value: strip non-digit/non-decimal characters
        let numeric = parse_amount(value);
        match numeric {
            Some(n) if n < 100_000.0       => "[AMOUNT_<100K\u{20ac}]".to_string(),
            Some(n) if n < 500_000.0       => "[AMOUNT_~500K\u{20ac}]".to_string(),
            Some(n) if n < 2_000_000.0     => "[AMOUNT_~1.5M\u{20ac}]".to_string(),
            Some(n) if n < 10_000_000.0    => "[AMOUNT_~5M\u{20ac}]".to_string(),
            Some(n) if n < 50_000_000.0    => "[AMOUNT_~25M\u{20ac}]".to_string(),
            Some(n) if n < 200_000_000.0   => "[AMOUNT_~100M\u{20ac}]".to_string(),
            Some(_)                        => "[AMOUNT_~500M\u{20ac}]".to_string(),
            None                           => "[AMOUNT_<100K\u{20ac}]".to_string(),
        }
    }

    fn generalize_addr(&self, value: &str) -> String {
        static RE_HOUSE_NUMBER: Lazy<Regex> = Lazy::new(|| {
            Regex::new(r"^\d+[a-zA-Z]?\s+").unwrap()
        });
        if let Some(m) = RE_HOUSE_NUMBER.find(value) {
            let remainder = &value[m.end()..];
            if remainder.trim().is_empty() {
                "[ADDR_GENERALIZED]".to_string()
            } else {
                remainder.to_string()
            }
        } else {
            "[ADDR_GENERALIZED]".to_string()
        }
    }

    // ── Relativize ───────────────────────────────────────────────────────

    fn relativize(
        &self,
        value: &str,
        entity_type: &str,
        category: EntityCategory,
        ctx: &mut SessionContext,
    ) -> String {
        let parsed = parse_date(value);
        match parsed {
            Some(date) => {
                if ctx.date_anchor.is_none() {
                    ctx.date_anchor = Some(date);
                    "[DATE_D0]".to_string()
                } else {
                    let anchor = ctx.date_anchor.unwrap();
                    let delta = (date - anchor).num_days();
                    if delta >= 0 {
                        format!("[DATE_D+{delta}]")
                    } else {
                        format!("[DATE_D{delta}]")
                    }
                }
            }
            None => {
                // Fallback to mask
                self.mask(value, entity_type, category, ctx)
            }
        }
    }
}

// ── ProfileAnonymizeResult ──────────────────────────────────────────────────

/// Result of profile-aware anonymization.
#[derive(Debug, Clone)]
pub struct ProfileAnonymizeResult {
    pub text: String,
    pub profile: AnonProfile,
    pub pii_count: usize,
    pub ner_degraded: bool,
    pub treatment_breakdown: HashMap<String, String>,
    pub kept_entities: Vec<String>,
}

/// Run profile-aware anonymization on text using L1 regex detection.
///
/// NER (L2) is not invoked here; `ner_degraded` is always `false` in L1-only mode
/// (consistent with the engine's existing behavior where degraded means "NER configured but failed").
pub fn anonymize_with_profile(
    text: &str,
    profile: AnonProfile,
    session_ctx: &mut SessionContext,
    engine: &TreatmentEngine,
) -> anyhow::Result<ProfileAnonymizeResult> {
    // L1 regex detection
    let detections = detect_legal_patterns_fr(text);

    // Deduplicate overlapping spans
    let detections = deduplicate(detections);

    let pii_count = detections.len();
    let ner_degraded = false; // L1-only mode: NER not attempted

    let mut treatment_breakdown = HashMap::new();
    let mut kept_entities = Vec::new();

    // Build replacement list: (start, end, replacement)
    let mut replacements: Vec<(usize, usize, String)> = Vec::with_capacity(detections.len());

    for det in &detections {
        let treatment = get_treatment(&profile, &det.category);
        let entity_type = det.category.placeholder_base();
        let replaced = engine.apply(
            &det.value,
            entity_type,
            det.category,
            treatment,
            session_ctx,
        );

        let treatment_name = match treatment {
            Treatment::Mask       => "mask",
            Treatment::Pseudonym  => "pseudonym",
            Treatment::Generalize => "generalize",
            Treatment::Relativize => "relativize",
            Treatment::Keep       => "keep",
        };
        treatment_breakdown.insert(entity_type.to_string(), treatment_name.to_string());

        if treatment == Treatment::Keep {
            kept_entities.push(det.value.clone());
        }

        replacements.push((det.start, det.end, replaced));
    }

    // Apply replacements in reverse order to preserve byte indices.
    // Guard both bounds AND UTF-8 char boundaries to avoid panics on
    // accented French text (é, à, ê, etc. are multi-byte in UTF-8).
    let mut result = text.to_string();
    replacements.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, end, replacement) in replacements {
        if start <= result.len()
            && end <= result.len()
            && start <= end
            && result.is_char_boundary(start)
            && result.is_char_boundary(end)
        {
            result.replace_range(start..end, &replacement);
        } else {
            tracing::warn!(start, end, "Skipping replacement: not on UTF-8 char boundary");
        }
    }

    Ok(ProfileAnonymizeResult {
        text: result,
        profile,
        pii_count,
        ner_degraded,
        treatment_breakdown,
        kept_entities,
    })
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Parse a numeric amount from a string containing digits, spaces, commas, dots.
///
/// Handles French-style amounts like "1 500 000 €", "1.500.000", "1,5M€".
fn parse_amount(value: &str) -> Option<f64> {
    // Check for multiplier suffixes first
    let lower = value.to_lowercase();

    // Extract digits and separators
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .collect();

    if cleaned.is_empty() {
        return None;
    }

    // If it has both dots and commas, determine which is decimal
    // French: 1.500.000,50 → dots are thousands, comma is decimal
    // Also handle: 1,500,000.50 → commas are thousands, dot is decimal
    let dot_count = cleaned.matches('.').count();
    let comma_count = cleaned.matches(',').count();

    let normalized = if dot_count > 1 {
        // Multiple dots = thousand separators, comma = decimal
        cleaned.replace('.', "").replace(',', ".")
    } else if comma_count > 1 {
        // Multiple commas = thousand separators, dot = decimal
        cleaned.replace(',', "")
    } else if dot_count == 1 && comma_count == 1 {
        // One of each: last one is decimal
        let dot_pos = cleaned.rfind('.').unwrap();
        let comma_pos = cleaned.rfind(',').unwrap();
        if comma_pos > dot_pos {
            // 1.500,50 → comma is decimal
            cleaned.replace('.', "").replace(',', ".")
        } else {
            // 1,500.50 → dot is decimal
            cleaned.replace(',', "")
        }
    } else if comma_count == 1 {
        // Single comma: could be decimal separator (French)
        cleaned.replace(',', ".")
    } else {
        cleaned
    };

    let base: f64 = normalized.parse().ok()?;

    // Check for multiplier keywords
    if lower.contains("milliard") {
        Some(base * 1_000_000_000.0)
    } else if lower.contains("million") || lower.contains("m€") || lower.contains("m ") {
        Some(base * 1_000_000.0)
    } else if lower.contains("k€") || lower.contains("k ") {
        Some(base * 1_000.0)
    } else {
        Some(base)
    }
}

/// Try parsing a date from common formats.
fn parse_date(value: &str) -> Option<NaiveDate> {
    let formats = [
        "%d/%m/%Y",
        "%Y-%m-%d",
        "%d.%m.%Y",
    ];
    for fmt in &formats {
        if let Ok(d) = NaiveDate::parse_from_str(value.trim(), fmt) {
            return Some(d);
        }
    }
    None
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_uses_bracket_format() {
        // [PERSON_1] not <PERSON_1>
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let result = engine.apply("Jean Dupont", "PERSON", EntityCategory::Per, Treatment::Mask, &mut ctx);
        assert_eq!(result, "[PERSON_1]");
    }

    #[test]
    fn test_mask_stable_same_session() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r1 = engine.apply("Jean Dupont", "PERSON", EntityCategory::Per, Treatment::Mask, &mut ctx);
        let r2 = engine.apply("Jean Dupont", "PERSON", EntityCategory::Per, Treatment::Mask, &mut ctx);
        assert_eq!(r1, r2);
        assert_eq!(r1, "[PERSON_1]");
    }

    #[test]
    fn test_mask_increments_counter() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r1 = engine.apply("Jean Dupont", "PERSON", EntityCategory::Per, Treatment::Mask, &mut ctx);
        let r2 = engine.apply("Marie Martin", "PERSON", EntityCategory::Per, Treatment::Mask, &mut ctx);
        assert_eq!(r1, "[PERSON_1]");
        assert_eq!(r2, "[PERSON_2]");
    }

    #[test]
    fn test_pseudonym_stable_same_session() {
        let engine = TreatmentEngine::new(vec!["Company Alpha".into(), "Company Beta".into()]);
        let mut ctx = SessionContext::new(AnonProfile::Deal);
        let r1 = engine.apply("Acme SA", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        let r2 = engine.apply("Acme SA", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        assert_eq!(r1, r2); // stable within session
        assert_eq!(r1, "Company Alpha");
    }

    #[test]
    fn test_pseudonym_round_robin() {
        let engine = TreatmentEngine::new(vec!["Alpha".into(), "Beta".into(), "Gamma".into()]);
        let mut ctx = SessionContext::new(AnonProfile::Deal);
        let r1 = engine.apply("Acme", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        let r2 = engine.apply("Globex", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        let r3 = engine.apply("Initech", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        assert_eq!(r1, "Alpha");
        assert_eq!(r2, "Beta");
        assert_eq!(r3, "Gamma");
    }

    #[test]
    fn test_pseudonym_empty_pool_falls_back_to_mask() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Deal);
        let r = engine.apply("Acme", "ORG", EntityCategory::Org, Treatment::Pseudonym, &mut ctx);
        assert_eq!(r, "[ORG_1]");
    }

    #[test]
    fn test_relativize_first_date_d0() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("15/06/2024", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        assert_eq!(r, "[DATE_D0]");
    }

    #[test]
    fn test_relativize_delta_30_days() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        engine.apply("15/06/2024", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        let r = engine.apply("15/07/2024", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        assert_eq!(r, "[DATE_D+30]");
    }

    #[test]
    fn test_relativize_negative_delta() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        engine.apply("15/06/2024", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        let r = engine.apply("01/06/2024", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        assert_eq!(r, "[DATE_D-14]");
    }

    #[test]
    fn test_relativize_unparseable_falls_back_to_mask() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("not-a-date", "DATE", EntityCategory::DateAbs, Treatment::Relativize, &mut ctx);
        assert_eq!(r, "[DATE_1]");
    }

    #[test]
    fn test_generalize_amount_1_5m() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        // 1 500 000 € → falls in 500K-2M bracket → [AMOUNT_~1.5M€]
        let r = engine.apply("1 500 000 €", "AMOUNT", EntityCategory::Amount, Treatment::Generalize, &mut ctx);
        assert_eq!(r, "[AMOUNT_~1.5M\u{20ac}]");
    }

    #[test]
    fn test_generalize_amount_small() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("50 000 €", "AMOUNT", EntityCategory::Amount, Treatment::Generalize, &mut ctx);
        assert_eq!(r, "[AMOUNT_<100K\u{20ac}]");
    }

    #[test]
    fn test_generalize_addr_strips_number() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("42 rue de la Paix", "ADDR", EntityCategory::Addr, Treatment::Generalize, &mut ctx);
        assert_eq!(r, "rue de la Paix");
    }

    #[test]
    fn test_generalize_addr_no_number() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("rue de la Paix", "ADDR", EntityCategory::Addr, Treatment::Generalize, &mut ctx);
        assert_eq!(r, "[ADDR_GENERALIZED]");
    }

    #[test]
    fn test_generalize_other_category() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("some data", "ORG", EntityCategory::Org, Treatment::Generalize, &mut ctx);
        assert_eq!(r, "[GENERALIZED]");
    }

    #[test]
    fn test_keep_returns_original() {
        let engine = TreatmentEngine::new(vec![]);
        let mut ctx = SessionContext::new(AnonProfile::Max);
        let r = engine.apply("SAS", "ORG_FORM", EntityCategory::OrgForm, Treatment::Keep, &mut ctx);
        assert_eq!(r, "SAS");
    }

    #[test]
    fn test_hkdf_key_length_is_32() {
        // Validate HKDF produces exactly 32 bytes, no zero-padding artifacts
        use hkdf::Hkdf;
        use sha2::Sha256;
        let hk = Hkdf::<Sha256>::new(None, b"test-secret");
        let mut key = [0u8; 32];
        hk.expand(b"gdpr-vault-key-v1", &mut key).unwrap();
        // key must not be all zeros (zero-padding artifact)
        assert_ne!(key, [0u8; 32]);
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn test_parse_amount_french() {
        assert!((parse_amount("1 500 000 €").unwrap() - 1_500_000.0).abs() < 1.0);
    }

    #[test]
    fn test_parse_amount_empty() {
        assert!(parse_amount("€").is_none());
    }

    #[test]
    fn test_parse_date_dmy_slash() {
        let d = parse_date("15/06/2024").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
    }

    #[test]
    fn test_parse_date_iso() {
        let d = parse_date("2024-06-15").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
    }

    #[test]
    fn test_parse_date_dot() {
        let d = parse_date("15.06.2024").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
    }
}
