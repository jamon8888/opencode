use std::sync::{Arc, Mutex};

use gdpr_mcp::{mcp::ingest_document, pii::PiiEngine, state::AppState};
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
async fn test_ingest_plain_text_extracts_and_anonymizes() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let result = ingest_document(
        &state,
        "tests/fixtures/sample.txt".to_string(),
        "fr".to_string(),
        "legitimate_interest".to_string(),
    )
    .await
    .unwrap();
    assert!(!result.document_id.is_empty());
    assert!(result.pii_count >= 1, "must detect at least 1 PII entity");
    assert!(
        !result.anonymized_preview.contains("jean.dupont@cabinet-legal.fr"),
        "preview must not contain raw email"
    );
}

#[tokio::test]
async fn test_ingest_records_entities_in_doc_audit() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let result = ingest_document(
        &state,
        "tests/fixtures/sample.txt".to_string(),
        "fr".to_string(),
        "contract".to_string(),
    )
    .await
    .unwrap();
    let rows = state.doc_audit.entities_for_doc(&result.document_id).unwrap();
    assert!(!rows.is_empty(), "doc_entity_map must have rows after ingest");
}

#[tokio::test]
async fn test_ingest_clean_file_zero_pii() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let result = ingest_document(
        &state,
        "tests/fixtures/sample_clean.txt".to_string(),
        "fr".to_string(),
        "legitimate_interest".to_string(),
    )
    .await
    .unwrap();
    assert_eq!(result.pii_count, 0, "clean file must have zero PII");
}

#[tokio::test]
async fn test_ingest_missing_file_returns_error() {
    let state = make_state();
    let result = ingest_document(
        &state,
        "tests/fixtures/no_such_file.pdf".to_string(),
        "auto".to_string(),
        "consent".to_string(),
    )
    .await;
    assert!(result.is_err(), "missing file must return Err");
}
