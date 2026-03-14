use gdpr_mcp::pii::{EnginePool, PiiEngine};
use tempfile::tempdir;

fn make_test_engine() -> PiiEngine {
    let dir = tempdir().unwrap();
    let vault = dir.path().join("v.vault").to_str().unwrap().to_string();
    std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-key-32-bytes-long-padding!!");
    PiiEngine::load_for_test(&vault).expect("engine must load")
}

#[test]
fn test_try_clone_produces_independent_engine() {
    let mut e1 = make_test_engine();
    let mut e2 = e1.try_clone().expect("try_clone must succeed");
    let mut nd1 = false;
    let mut nd2 = false;
    let r1 = e1.anonymize("alice@example.com", &mut nd1).unwrap();
    let r2 = e2.anonymize("alice@example.com", &mut nd2).unwrap();
    // Pseudonyms must be the same because Vault is shared
    assert_eq!(r1.text, r2.text, "shared vault must produce identical pseudonyms");
}

#[test]
fn test_anonymize_batch_produces_one_result_per_input() {
    let mut engine = make_test_engine();
    let texts = &["alice@example.com", "no PII here", "bob@test.org"];
    let mut nd = false;
    let results = engine.anonymize_batch(texts, &mut nd).unwrap();
    assert_eq!(results.len(), 3);
    assert!(!results[0].text.contains("alice@example.com"));
    assert_eq!(results[1].text, "no PII here"); // passthrough
    assert!(!results[2].text.contains("bob@test.org"));
}

#[test]
fn test_anonymize_batch_ner_degraded_when_no_model() {
    let mut engine = make_test_engine(); // no model → L1 only
    let mut nd = false;
    engine.anonymize_batch(&["hello"], &mut nd).unwrap();
    assert!(nd, "ner_degraded must be true when no model loaded");
}

// ── EnginePool tests ──────────────────────────────────────────────────────────

#[test]
fn test_engine_pool_creates_n_slots() {
    let seed   = make_test_engine();
    let pool   = EnginePool::new(seed, 4).expect("pool must build");
    let result = pool.anonymize_batch(&["no PII here"]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0], "no PII here");
}

#[test]
fn test_engine_pool_anonymizes_multiple_texts() {
    let seed = make_test_engine();
    let pool = EnginePool::new(seed, 2).expect("pool must build");
    let inputs = &["Contact alice@example.com", "hello world", "bob@test.org info"];
    let results = pool.anonymize_batch(inputs);
    assert_eq!(results.len(), 3);
    assert!(!results[0].contains("alice@example.com"), "alice must be pseudonymized");
    assert_eq!(results[1], "hello world", "clean text unchanged");
    assert!(!results[2].contains("bob@test.org"), "bob must be pseudonymized");
}

#[test]
fn test_engine_pool_shared_vault_produces_consistent_pseudonyms() {
    let seed = make_test_engine();
    let pool = EnginePool::new(seed, 4).expect("pool must build");
    let r1 = pool.anonymize_batch(&["alice@example.com"]);
    let r2 = pool.anonymize_batch(&["alice@example.com"]);
    assert_eq!(r1[0], r2[0], "shared vault must produce identical pseudonyms across calls");
}
