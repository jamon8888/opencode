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

-- ── Extend gdpr_audit with multi-tenant correlation (GDPR Art. 30) ────────────
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS tenant_id  String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS request_id String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS api_key_id String DEFAULT '';

-- ── Usage events (billing metering — high-volume, append-only) ───────────────
CREATE TABLE IF NOT EXISTS usage_events (
    id            UUID     DEFAULT generateUUIDv4(),
    tenant_id     String,
    api_key_id    String,
    request_id    String,
    event_type    Enum8('ingest'=1,'search'=2,'anonymize'=3,'ai_chat'=4,'delete'=5),
    document_id   String   DEFAULT '',
    chars_in      UInt64   DEFAULT 0,
    chars_out     UInt64   DEFAULT 0,
    doc_count     UInt32   DEFAULT 1,
    chunk_count   UInt32   DEFAULT 0,
    ai_tokens_in  UInt32   DEFAULT 0,
    ai_tokens_out UInt32   DEFAULT 0,
    ner_tier      Enum8('l1'=1,'l1_l2'=2) DEFAULT 'l1',
    latency_ms    UInt32   DEFAULT 0,
    ts            DateTime DEFAULT now()
) ENGINE = MergeTree()
  PARTITION BY toYYYYMM(ts)
  ORDER BY (tenant_id, ts)
  SETTINGS index_granularity = 8192;

-- ── Billing snapshots (materialized view target) ──────────────────────────────
CREATE TABLE IF NOT EXISTS billing_snapshots (
    tenant_id           String,
    period_ym           UInt32,
    total_docs          UInt64,
    total_chars_in      UInt64,
    total_rag_queries   UInt64,
    total_ai_tokens_in  UInt64,
    total_ai_tokens_out UInt64,
    computed_at         DateTime DEFAULT now()
) ENGINE = ReplacingMergeTree(computed_at)
  ORDER BY (tenant_id, period_ym);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_billing_snapshots
TO billing_snapshots AS
SELECT
    tenant_id,
    toYYYYMM(ts)                             AS period_ym,
    countIf(event_type = 'ingest')           AS total_docs,
    sum(chars_in)                            AS total_chars_in,
    countIf(event_type = 'search')           AS total_rag_queries,
    sum(toUInt64(ai_tokens_in))              AS total_ai_tokens_in,
    sum(toUInt64(ai_tokens_out))             AS total_ai_tokens_out,
    now()                                    AS computed_at
FROM usage_events
GROUP BY tenant_id, toYYYYMM(ts);
