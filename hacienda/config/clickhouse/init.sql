-- TensorZero inference logging database (auto-migrated by TensorZero on startup)
CREATE DATABASE IF NOT EXISTS tensorzero;

-- ── GDPR Art. 30 immutable audit trail ───────────────────────────────────────
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
    ai_act_risk_level  String DEFAULT 'low',  -- AI Act Art. 9: low | medium | high | critical
    decision_explanation String DEFAULT '',   -- AI Act Art. 13 transparency
    ts_unix            UInt64 DEFAULT toUnixTimestamp(now())
)
ENGINE = ReplacingMergeTree()
ORDER BY (document_id, ts_unix)
SETTINGS index_granularity = 8192;

-- ── Session vault tracking ────────────────────────────────────────────────────
-- Tracks per-session lifecycle: token consumption, document scope, timing.

CREATE TABLE IF NOT EXISTS gdpr_sessions (
    session_id  String,
    api_key_id  String,
    started_at  UInt64,
    ended_at    UInt64 DEFAULT 0,
    token_count UInt32,
    doc_ids     Array(String)
) ENGINE = MergeTree() ORDER BY (session_id, started_at);

-- ── Profile usage telemetry ───────────────────────────────────────────────────
-- Auto-aggregates usage_count via SummingMergeTree — no application-level aggregation needed.

CREATE TABLE IF NOT EXISTS gdpr_profiles (
    profile_name String,
    api_key_id   String,
    usage_count  UInt64,
    avg_pii_count Float32,
    ts_unix      UInt64 DEFAULT toUnixTimestamp(now())
) ENGINE = SummingMergeTree(usage_count) ORDER BY (profile_name, api_key_id);

-- ── API key audit trail ───────────────────────────────────────────────────────
-- Deduplicated via ReplacingMergeTree — tracks rotation, creation, revocation.

CREATE TABLE IF NOT EXISTS gdpr_keys (
    key_id     String,
    event      String,
    api_key_id String,
    ts_unix    UInt64 DEFAULT toUnixTimestamp(now())
) ENGINE = ReplacingMergeTree() ORDER BY (key_id, ts_unix);

-- ── Metered usage per API key (for billing) ───────────────────────────────────
-- SummingMergeTree accumulates all numeric columns automatically.

CREATE TABLE IF NOT EXISTS gdpr_billing (
    api_key_id      String,
    month           String,
    tokens_in       UInt64,
    tokens_out      UInt64,
    documents_count UInt32,
    requests_count  UInt32
) ENGINE = SummingMergeTree(tokens_in, tokens_out, documents_count, requests_count)
ORDER BY (api_key_id, month);
