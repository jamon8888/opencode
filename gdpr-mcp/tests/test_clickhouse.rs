use gdpr_mcp::clients::clickhouse::{ClickHouseClient, GdprAuditRow};

fn test_row() -> GdprAuditRow {
    GdprAuditRow {
        document_id:        "doc-123".into(),
        action:             "ingest".into(),
        pii_count_before:   5,
        pii_count_after:    0,
        ner_degraded:       false,
        processing_time_ms: 12,
        legal_basis:        "contract".into(),
        user_id:            "u1".into(),
        model_version:      "gliner-pii-edge-v1.0".into(),
    }
}

#[test]
fn test_row_serializes_to_json() {
    let row  = test_row();
    let json = serde_json::to_string(&row).expect("must serialize");
    assert!(json.contains("\"document_id\":\"doc-123\""));
    assert!(json.contains("\"action\":\"ingest\""));
    assert!(json.contains("\"pii_count_before\":5"));
}

#[tokio::test]
async fn test_buffer_fills_and_drops_oldest_on_overflow() {
    // Use a non-existent ClickHouse URL to force immediate CB failure
    let client = ClickHouseClient::new("http://127.0.0.1:19999");

    // Fire 10 001 records — first one must be dropped when capacity is exceeded
    for i in 0..10_001u32 {
        let mut row = test_row();
        row.pii_count_before = i;
        client.record(row).await;
    }

    // Ring buffer size must be capped at 10 000
    let buf_len = client.buffer_len();
    assert_eq!(buf_len, 10_000, "buffer must not exceed BUFFER_CAP");
}

#[tokio::test]
async fn test_record_does_not_panic_on_unreachable_server() {
    let client = ClickHouseClient::new("http://127.0.0.1:19999");
    // Must complete without panic — CB absorbs the failure
    client.record(test_row()).await;
    client.record(test_row()).await;
}
