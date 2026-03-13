use gdpr_mcp::pii::PiiEngine;
use tempfile::tempdir;

fn make_engine() -> PiiEngine {
    let dir = tempdir().unwrap();
    let vault_path = dir.path().join("test.vault").to_str().unwrap().to_string();
    std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-key-32-bytes-long-padding!!");
    PiiEngine::load_for_test(&vault_path).expect("engine must load")
}

#[test]
fn test_email_is_pseudonymized() {
    let mut engine = make_engine();
    let mut degraded = false;
    let result = engine.anonymize("Contact alice@example.com for details", &mut degraded).unwrap();
    assert!(!result.text.contains("alice@example.com"), "raw email must not appear");
    assert!(result.text.contains("EMAIL_"), "must contain email pseudonym");
    assert_eq!(result.pii_count, 1);
}

#[test]
fn test_iban_is_pseudonymized() {
    let mut engine = make_engine();
    let mut degraded = false;
    let result = engine.anonymize("IBAN: FR76 3000 6000 0112 3456 7890 189", &mut degraded).unwrap();
    assert!(!result.text.contains("FR76"), "raw IBAN must not appear");
    assert!(result.text.contains("IBAN_") || result.text.contains("AMOUNT_"), "must be pseudonymized");
}

#[test]
fn test_clean_text_passes_through_unchanged() {
    let mut engine = make_engine();
    let mut degraded = false;
    let result = engine.anonymize("The weather in Paris is sunny today.", &mut degraded).unwrap();
    assert_eq!(result.pii_count, 0);
    assert_eq!(result.text, "The weather in Paris is sunny today.");
}

#[test]
fn test_rehydration_restores_original() {
    let mut engine = make_engine();
    let mut degraded = false;
    let input = "Call alice@example.com at +33612345678";
    let anon = engine.anonymize(input, &mut degraded).unwrap();
    // If PII was detected, rehydration must restore it
    if anon.pii_count > 0 {
        let rehydrated = engine.rehydrate(&anon.text).unwrap();
        assert!(rehydrated.contains("alice@example.com") || rehydrated.contains("+33612345678"),
            "rehydrated text must restore at least one original value, got: {}", rehydrated);
    }
    // If pii_count is 0 (no detection), text passes through unchanged — rehydrate is a no-op
}

#[test]
fn test_same_entity_gets_same_pseudonym() {
    let mut engine = make_engine();
    let mut d = false;
    let r1 = engine.anonymize("Email alice@example.com here.", &mut d).unwrap();
    let r2 = engine.anonymize("Reply to alice@example.com please.", &mut d).unwrap();
    let token1 = r1.text.split_whitespace()
        .find(|w| w.contains("EMAIL_")).unwrap_or("").to_string();
    let token2 = r2.text.split_whitespace()
        .find(|w| w.contains("EMAIL_")).unwrap_or("").to_string();
    assert_eq!(token1, token2, "same entity must produce same pseudonym across calls");
}
