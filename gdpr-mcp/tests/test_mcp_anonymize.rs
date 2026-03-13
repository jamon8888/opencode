use std::sync::{Arc, Mutex};

use gdpr_mcp::{
    mcp::{anonymize_text, deanonymize_response},
    pii::PiiEngine,
    state::AppState,
};
use tempfile::tempdir;

static ENGINE_LOCK: Mutex<()> = Mutex::new(());

fn make_state() -> Arc<AppState> {
    let dir = tempdir().unwrap();
    let vault_path = dir.path().join("vault.db").to_str().unwrap().to_string();
    let db_path = dir.path().join("docs.db").to_str().unwrap().to_string();
    std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-key-32-bytes-long-padding!!");
    let engine = PiiEngine::load_for_test(&vault_path).expect("engine must load");
    let db = rusqlite::Connection::open(db_path).expect("db must open");
    Arc::new(AppState::new(engine, db).expect("state must init"))
}

#[tokio::test]
async fn test_anonymize_text_removes_email() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let result = anonymize_text(&state, "Send to bob@acme.com please".to_string()).await.unwrap();
    assert!(!result.anonymized.contains("bob@acme.com"));
    assert!(result.pii_count >= 1);
}

#[tokio::test]
async fn test_anonymize_text_clean_input() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let input = "The meeting is on Tuesday.".to_string();
    let result = anonymize_text(&state, input.clone()).await.unwrap();
    assert_eq!(result.anonymized, input);
    assert_eq!(result.pii_count, 0);
}

#[tokio::test]
async fn test_deanonymize_roundtrip() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let anon =
        anonymize_text(&state, "Contact alice@example.com for billing".to_string()).await.unwrap();
    let deano = deanonymize_response(&state, anon.anonymized.clone()).await.unwrap();
    assert!(deano.text.contains("alice@example.com"), "must restore original email");
}

#[tokio::test]
async fn test_deanonymize_no_pseudonyms_passes_through() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let plain = "No pseudonyms here at all.".to_string();
    let result = deanonymize_response(&state, plain.clone()).await.unwrap();
    assert_eq!(result.text, plain);
}
