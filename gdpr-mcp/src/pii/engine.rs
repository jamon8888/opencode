//! PII detection and pseudonymization engine.
//!
//! Uses cloakpipe-core for L1 pattern detection and optional L2 NER.
//! Provides consistent pseudonymization via an AES-256-GCM vault.

use cloakpipe_core::{
    config::DetectionConfig,
    detector::Detector,
    replacer::Replacer,
    rehydrator::Rehydrator,
    vault::Vault,
    DetectedEntity,
};
use std::sync::{Arc, Mutex};

/// Result of anonymizing a piece of text.
#[derive(Debug, Clone)]
pub struct AnonymizeResult {
    /// The text with all PII replaced by pseudo-tokens.
    pub text: String,
    /// All entities that were detected and replaced.
    pub entities: Vec<DetectedEntity>,
    /// Whether NER (L2) was unavailable and fell back to L1 only.
    pub ner_degraded: bool,
    /// Total number of PII entities detected and replaced.
    pub pii_count: usize,
}

/// The PII detection and pseudonymization engine.
pub struct PiiEngine {
    /// L1 pattern detector (regex + financial). Always runs.
    detector: Detector,
    /// Shared vault for consistent pseudonymization across calls.
    vault: Arc<Mutex<Vault>>,
    /// Whether L2/NER is configured. False = L1 only (degraded mode).
    has_ner: bool,
}

impl PiiEngine {
    /// Create an engine suitable for unit tests.
    ///
    /// Uses L1 pattern detection only (no NER model required).
    /// Reads the vault key from the `CLOAKPIPE_VAULT_KEY` env var,
    /// padding or truncating to exactly 32 bytes. Falls back to a
    /// zero-filled key if the env var is not set.
    pub fn load_for_test(vault_path: &str) -> anyhow::Result<Self> {
        let config = Self::default_detection_config()?;
        let detector = Detector::from_config(&config)?;

        let key = Self::key_from_env();
        let vault = Vault::open(vault_path, key)?;

        Ok(Self {
            detector,
            vault: Arc::new(Mutex::new(vault)),
            has_ner: false,
        })
    }

    /// Create an engine from explicit configuration.
    pub fn new(config: &DetectionConfig, vault: Vault, has_ner: bool) -> anyhow::Result<Self> {
        let detector = Detector::from_config(config)?;
        Ok(Self {
            detector,
            vault: Arc::new(Mutex::new(vault)),
            has_ner,
        })
    }

    /// Anonymize text, replacing all detected PII with pseudo-tokens.
    ///
    /// If no PII is detected the original text is returned unchanged
    /// (guarantees `test_clean_text_passes_through_unchanged`).
    ///
    /// `ner_degraded` is set to `true` when this engine has no L2 NER
    /// (i.e., was built with `load_for_test`). Callers may expose this
    /// flag in responses so clients know detection may be less precise.
    pub fn anonymize(
        &mut self,
        text: &str,
        ner_degraded: &mut bool,
    ) -> anyhow::Result<AnonymizeResult> {
        // L1 detection (patterns + financial)
        let entities = self.detector.detect(text)?;

        // ner_degraded = true only when L2/NER is NOT configured.
        *ner_degraded = !self.has_ner;

        if entities.is_empty() {
            // Fast-path: return original text unchanged
            return Ok(AnonymizeResult {
                text: text.to_string(),
                entities: vec![],
                ner_degraded: *ner_degraded,
                pii_count: 0,
            });
        }

        let pii_count = entities.len();

        let mut vault = self.vault.lock().map_err(|_| anyhow::anyhow!("vault lock poisoned"))?;
        let pseudonymized = Replacer::pseudonymize(text, &entities, &mut vault)?;

        Ok(AnonymizeResult {
            text: pseudonymized.text,
            entities: pseudonymized.entities,
            ner_degraded: *ner_degraded,
            pii_count,
        })
    }

    /// Rehydrate a text that was previously anonymized, restoring original values.
    pub fn rehydrate(&self, text: &str) -> anyhow::Result<String> {
        let vault = self.vault.lock().map_err(|_| anyhow::anyhow!("vault lock poisoned"))?;
        let result = Rehydrator::rehydrate(text, &vault)?;
        Ok(result.text)
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Build a `DetectionConfig` with sensible defaults via serde.
    /// All fields in `DetectionConfig` have `#[serde(default)]` annotations
    /// so deserializing from `{}` is always valid.
    fn default_detection_config() -> anyhow::Result<DetectionConfig> {
        // Enable secrets, emails, financial, dates (all true by default in cloakpipe).
        // phone_numbers enabled for the rehydration test ("+33 6 12 34 56 78").
        // Custom patterns include IBAN (EU bank accounts) since cloakpipe has no
        // built-in IBAN detection.
        let json = r#"{
            "secrets": true,
            "financial": true,
            "dates": true,
            "emails": true,
            "phone_numbers": true,
            "ip_addresses": false,
            "urls_internal": false,
            "custom": {
                "patterns": [
                    {
                        "name": "iban",
                        "regex": "[A-Z]{2}\\d{2}(?:\\s?[A-Z0-9]{4}){4,7}(?:\\s?[A-Z0-9]{1,4})?",
                        "category": "IBAN"
                    }
                ]
            }
        }"#;
        let config: DetectionConfig = serde_json::from_str(json)?;
        Ok(config)
    }

    /// Derive a 32-byte AES-256 key from the `CLOAKPIPE_VAULT_KEY` env var.
    /// Pads with zeros or truncates to exactly 32 bytes.
    fn key_from_env() -> Vec<u8> {
        let raw = std::env::var("CLOAKPIPE_VAULT_KEY").unwrap_or_default();
        let mut key = raw.into_bytes();
        key.resize(32, 0u8);
        key
    }
}
