use std::sync::{Arc, Mutex};

use gdpr_mcp::{mcp::{audit_report, ingest_document, list_documents}, pii::PiiEngine, state::AppState};
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
async fn test_list_documents_empty() {
    let state = make_state();
    let result = list_documents(&state).await.unwrap();
    assert!(result.documents.is_empty());
}

#[tokio::test]
async fn test_list_documents_after_ingest() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    ingest_document(
        &state,
        "tests/fixtures/sample.txt".into(),
        "fr".into(),
        "consent".into(),
    )
    .await
    .unwrap();
    let result = list_documents(&state).await.unwrap();
    assert_eq!(result.documents.len(), 1);
}

#[tokio::test]
async fn test_audit_report_no_filter() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    ingest_document(
        &state,
        "tests/fixtures/sample.txt".into(),
        "fr".into(),
        "contract".into(),
    )
    .await
    .unwrap();
    let result = audit_report(&state, None, None, None).await;
    assert!(result.is_ok());
    assert!(result.unwrap().total_operations >= 1);
}
