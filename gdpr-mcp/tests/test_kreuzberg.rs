use gdpr_mcp::extraction::{extract_document, extract_document_bytes};
use std::path::Path;

#[tokio::test]
async fn test_extract_plain_text() {
    let path = Path::new("tests/fixtures/sample.txt");
    let result = extract_document(path, "fr").await.unwrap();
    assert!(!result.content.is_empty(), "content must not be empty");
    assert!(result.content.contains("Jean Dupont"), "must extract PII text");
}

#[tokio::test]
async fn test_extract_clean_text() {
    let path = Path::new("tests/fixtures/sample_clean.txt");
    let result = extract_document(path, "fr").await.unwrap();
    assert!(!result.content.is_empty());
    assert!(result.quality_score >= 0.0 && result.quality_score <= 1.0);
}

#[tokio::test]
async fn test_extract_nonexistent_file_returns_error() {
    let path = Path::new("tests/fixtures/does_not_exist.pdf");
    let result = extract_document(path, "auto").await;
    assert!(result.is_err(), "must return Err for missing file");
}

#[tokio::test]
async fn test_low_quality_document_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("empty.txt");
    std::fs::write(&p, "   \n\n   ").unwrap();
    let result = extract_document(&p, "auto").await;
    assert!(result.is_ok(), "should not hard-fail on low-quality doc");
}

#[tokio::test]
async fn test_extract_bytes_plain_text() {
    let data = std::fs::read("tests/fixtures/sample.txt").unwrap();
    let result = extract_document_bytes(&data, "text/plain", "fr").await.unwrap();
    assert!(result.content.contains("Jean Dupont"));
}
