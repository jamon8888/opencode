use std::sync::{Arc, Mutex};

use gdpr_mcp::{
    mcp::{delete_document, ingest_document, list_documents},
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
async fn test_delete_removes_from_doc_audit() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let ingested = ingest_document(
        &state,
        "tests/fixtures/sample.txt".into(),
        "fr".into(),
        "consent".into(),
    )
    .await
    .unwrap();

    let before = list_documents(&state).await.unwrap();
    assert_eq!(before.total, 1);

    let result =
        delete_document(&state, ingested.document_id.clone(), "test erasure".to_string())
            .await
            .unwrap();
    assert!(result.success);
    assert_eq!(result.document_id, ingested.document_id);

    let after = list_documents(&state).await.unwrap();
    assert_eq!(after.total, 0, "document must be gone after Art. 17 erasure");
}

#[tokio::test]
async fn test_delete_nonexistent_document_is_ok() {
    let state = make_state();
    let result = delete_document(&state, "does-not-exist".to_string(), "test".to_string()).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().rows_deleted, 0);
}

#[tokio::test]
async fn test_delete_twice_is_idempotent() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let ingested = ingest_document(
        &state,
        "tests/fixtures/sample.txt".into(),
        "fr".into(),
        "consent".into(),
    )
    .await
    .unwrap();
    delete_document(&state, ingested.document_id.clone(), "first".to_string()).await.unwrap();
    let r2 =
        delete_document(&state, ingested.document_id, "second".to_string()).await.unwrap();
    assert_eq!(r2.rows_deleted, 0, "second delete must be a no-op");
}
