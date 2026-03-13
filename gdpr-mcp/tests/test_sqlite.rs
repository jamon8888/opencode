use gdpr_mcp::audit::{DocAuditDb, DocEntityRow};
use tempfile::tempdir;

fn make_db() -> DocAuditDb {
    let dir = tempdir().unwrap();
    DocAuditDb::open(dir.path().join("test.db").to_str().unwrap()).unwrap()
}

#[test]
fn test_record_and_retrieve_entities() {
    let db = make_db();
    db.record_entities(
        "doc-1",
        &[DocEntityRow {
            entity_type: "EMAIL".to_string(),
            pseudonym: "EMAIL_1".to_string(),
            detection_layer: "l1_pattern".to_string(),
            confidence: Some(1.0),
            ner_degraded: false,
        }],
    )
    .unwrap();
    let rows = db.entities_for_doc("doc-1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pseudonym, "EMAIL_1");
}

#[test]
fn test_delete_document_removes_rows() {
    let db = make_db();
    db.record_entities(
        "doc-42",
        &[DocEntityRow {
            entity_type: "PERSON".to_string(),
            pseudonym: "PERSON_1".to_string(),
            detection_layer: "l2_ner".to_string(),
            confidence: Some(0.9),
            ner_degraded: false,
        }],
    )
    .unwrap();
    let deleted = db.delete_document("doc-42").unwrap();
    assert_eq!(deleted, 1);
    assert!(db.entities_for_doc("doc-42").unwrap().is_empty());
}

#[test]
fn test_list_documents_returns_distinct_ids() {
    let db = make_db();
    db.record_entities(
        "doc-A",
        &[DocEntityRow {
            entity_type: "IBAN".to_string(),
            pseudonym: "IBAN_1".to_string(),
            detection_layer: "l1_pattern".to_string(),
            confidence: None,
            ner_degraded: false,
        }],
    )
    .unwrap();
    db.record_entities(
        "doc-B",
        &[DocEntityRow {
            entity_type: "EMAIL".to_string(),
            pseudonym: "EMAIL_2".to_string(),
            detection_layer: "l1_pattern".to_string(),
            confidence: None,
            ner_degraded: false,
        }],
    )
    .unwrap();
    let docs = db.list_documents().unwrap();
    assert_eq!(docs.len(), 2);
}

#[test]
fn test_delete_nonexistent_returns_zero() {
    let db = make_db();
    let deleted = db.delete_document("does-not-exist").unwrap();
    assert_eq!(deleted, 0);
}
