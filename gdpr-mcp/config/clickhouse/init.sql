-- GDPR Art. 30 immutable audit trail
-- Engine: ReplacingMergeTree ensures idempotent inserts; ORDER BY (document_id, ts_unix) for efficient range queries.

CREATE TABLE IF NOT EXISTS gdpr_audit (
    document_id        String,
    action             String,           -- 'ingest' | 'query' | 'deanonymize' | 'delete'
    pii_count_before   UInt32,
    pii_count_after    UInt32,
    ner_degraded       UInt8,            -- 0 = L1+L2, 1 = L1 only
    processing_time_ms UInt32,
    legal_basis        String,           -- GDPR Art. 6 basis
    user_id            String,
    model_version      String,
    ts_unix            UInt64 DEFAULT toUnixTimestamp(now())
)
ENGINE = ReplacingMergeTree()
ORDER BY (document_id, ts_unix)
SETTINGS index_granularity = 8192;
