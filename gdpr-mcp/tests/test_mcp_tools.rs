/// Integration tests for the full ingest → search → audit → delete lifecycle.
///
/// cloakpipe-core's Detector holds a global lock — tests that call
/// `engine.anonymize` must run sequentially. `ENGINE_LOCK` serialises them.
use std::sync::{Arc, Mutex};

use gdpr_mcp::{
    audit::{now_unix, AuditEvent, AuditLog},
    pii::PiiEngine,
    state::AppState,
};
use tempfile::tempdir;
use uuid::Uuid;

// Serialize tests that call the PII engine (cloakpipe global lock).
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

// ── Pipeline helpers (mirror what the MCP tools do internally) ───────────────

fn ingest(state: &AppState, text: &str) -> (String, usize) {
    let mut engine = state.pii_engine.lock().unwrap();
    let mut ner_degraded = false;
    let result = engine.anonymize(text, &mut ner_degraded).unwrap();
    drop(engine);

    let doc_id = Uuid::new_v4().to_string();
    let ts = now_unix() as i64;

    let db = state.db.lock().unwrap();
    db.execute(
        "INSERT INTO documents (id, anon_text, pii_count, ner_degraded, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![doc_id, result.text, result.pii_count as i64, 0i64, ts],
    )
    .unwrap();
    drop(db);

    AuditLog::new(Arc::clone(&state.db))
        .record(AuditEvent::Ingest { doc_id: &doc_id, pii_count: result.pii_count });

    (doc_id, result.pii_count)
}

fn search(state: &AppState, query: &str, limit: i64) -> Vec<(String, String)> {
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare(
            "SELECT id, anon_text FROM documents
             WHERE anon_text LIKE ?1 ESCAPE '\\' LIMIT ?2",
        )
        .unwrap();
    stmt.query_map(rusqlite::params![pattern, limit], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn delete(state: &AppState, doc_id: &str) -> bool {
    let db = state.db.lock().unwrap();
    let affected = db
        .execute("DELETE FROM documents WHERE id = ?1", rusqlite::params![doc_id])
        .unwrap();
    drop(db);
    if affected > 0 {
        AuditLog::new(Arc::clone(&state.db)).record(AuditEvent::Delete { doc_id });
        true
    } else {
        false
    }
}

fn audit_events(state: &AppState, doc_id: &str) -> Vec<String> {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare("SELECT event_type FROM audit_log WHERE doc_id = ?1 ORDER BY id ASC")
        .unwrap();
    stmt.query_map(rusqlite::params![doc_id], |row| row.get::<_, String>(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_schema_initialised() {
    let state = make_state();
    let db = state.db.lock().unwrap();
    let doc_count: i64 =
        db.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0)).unwrap();
    let audit_count: i64 =
        db.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
    assert_eq!(doc_count, 0);
    assert_eq!(audit_count, 0);
}

#[test]
fn test_ingest_pii_text_stores_anonymized() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let (doc_id, pii_count) = ingest(&state, "Contact alice@example.com for the contract.");

    assert!(!doc_id.is_empty());
    assert_eq!(pii_count, 1, "email must be detected as 1 PII entity");

    let db = state.db.lock().unwrap();
    let anon: String = db
        .query_row(
            "SELECT anon_text FROM documents WHERE id = ?1",
            rusqlite::params![doc_id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!anon.contains("alice@example.com"), "raw email must not be stored");
}

#[test]
fn test_ingest_clean_text_has_zero_pii() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let (_, pii_count) = ingest(&state, "The conference is in Paris next week.");
    assert_eq!(pii_count, 0);
}

#[test]
fn test_search_finds_ingested_doc() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let (doc_id, _) = ingest(&state, "The project deadline is next Monday.");
    let hits = search(&state, "deadline", 10);
    assert!(hits.iter().any(|(id, _)| id == &doc_id));
}

#[test]
fn test_search_does_not_expose_raw_pii() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    ingest(&state, "Reply to bob@secret.org about the merger.");
    let hits = search(&state, "merger", 10);
    for (_, anon_text) in &hits {
        assert!(
            !anon_text.contains("bob@secret.org"),
            "raw email must not appear in results: {anon_text}"
        );
    }
}

#[test]
fn test_search_wildcard_escaping() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    ingest(&state, "Sensitive document about financing.");
    // Bare % is escaped to \% and must not match all rows
    let hits = search(&state, "%", 100);
    assert_eq!(hits.len(), 0, "bare % must not match all documents after escaping");
}

#[test]
fn test_delete_removes_doc_from_search() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let (doc_id, _) = ingest(&state, "A neutral document about the weather.");

    assert!(search(&state, "weather", 10).iter().any(|(id, _)| id == &doc_id));

    assert!(delete(&state, &doc_id));
    assert!(!search(&state, "weather", 10).iter().any(|(id, _)| id == &doc_id));
}

#[test]
fn test_delete_nonexistent_returns_false() {
    assert!(!delete(&make_state(), "no-such-id"));
}

#[test]
fn test_audit_log_records_ingest_then_delete() {
    let _g = ENGINE_LOCK.lock().unwrap();
    let state = make_state();
    let (doc_id, _) = ingest(&state, "Audit trail test document.");
    delete(&state, &doc_id);

    let events = audit_events(&state, &doc_id);
    assert!(events.contains(&"ingest".to_string()));
    assert!(events.contains(&"delete".to_string()));

    let ingest_pos = events.iter().position(|e| e == "ingest").unwrap();
    let delete_pos = events.iter().position(|e| e == "delete").unwrap();
    assert!(ingest_pos < delete_pos, "ingest must precede delete in audit log");
}

#[test]
fn test_audit_delete_not_written_for_nonexistent_doc() {
    let state = make_state();
    delete(&state, "phantom-id");
    let events = audit_events(&state, "phantom-id");
    assert!(!events.contains(&"delete".to_string()));
}
