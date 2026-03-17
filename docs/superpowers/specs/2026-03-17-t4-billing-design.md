# T4 — gdpr-billing Crate: Full Rewrite Design

**Date:** 2026-03-17
**Status:** Approved (v2 — post spec review)
**Scope:** Complete rewrite of the `gdpr-billing` crate skeleton into production-grade billing with millicent integer arithmetic, plan-tiered caps, overage calculation, and full ClickHouse metering integration.

---

## Context

The `gdpr-billing` crate currently contains a skeleton with:
- `meter.rs` — only `UsageRecord` (tokens + requests, no docs/chars/RAG)
- `cap.rs` — token/request caps only
- `client.rs` — own ClickHouse HTTP client writing to old `gdpr_billing` table
- `invoice.rs` — hardcoded flat rate `€2/M tokens`, no plan tiers

All skeleton files are replaced. `client.rs` is deleted.

Cross-crate gaps also fixed:
- `Plan` lives in `gdpr-api::state` but belongs in `gdpr-billing`
- `gdpr-api` has no dependency on `gdpr-billing`
- `middleware/meter.rs` is a logging stub (deferred to "T8")
- `auth.rs` hardcodes `Plan::Starter` for all keys
- `handlers/usage.rs` returns zeros
- `config/clickhouse/init.sql` missing `usage_events`, `billing_snapshots`, and `gdpr_audit` ALTER statements

---

## Design Decision: Millicent Integer Arithmetic

All monetary values stored and computed as `u64` EUR millicents (1 millicent = €0.00001). No floating-point arithmetic in the billing pipeline. `f64` conversion occurs only at the JSON serialization boundary in `invoice.rs`.

Rationale: Eliminates floating-point accumulation errors at scale. Matches industry standard (Stripe, AWS). No new dependencies required. Unit prices in the spec already use this encoding (`8_000` = €0.08).

---

## ClickHouseClient Clarification

There are two `ClickHouseClient` types in the workspace:

| Client | Location | Generic write? | Constructor |
|--------|----------|---------------|-------------|
| `gdpr_core::clients::ClickHouseClient` | `gdpr-core` | ✅ `write_batch_table(&str, rows)` | `::new(url: &str)` |
| `crate::clients::ClickHouseClient` | `gdpr-api` | ❌ only `write_audit_row` | `::new(base_url, http) -> Arc<Self>` |

**`Meter` uses `gdpr_core::clients::ClickHouseClient`** — gdpr-billing already depends on gdpr-core, which provides the generic `write_batch_table` method needed to write `UsageEvent` rows to `usage_events`.

In `gdpr-api/src/main.rs`, a separate `gdpr_core::clients::ClickHouseClient` is constructed for the `Meter` (pointing at the same ClickHouse URL as the existing gdpr-api client). Two clients, one ClickHouse — this is fine and avoids any coupling.

---

## ClickHouse Table Naming Convention

`gdpr-core`'s `write_batch_table` and `init.sql` both use **bare table names without a database prefix** (e.g., `gdpr_audit`, `gdpr_sessions`, `gdpr_billing`). The gdpr-api client's hardcoded `gdpr.gdpr_audit` is an existing inconsistency that is not changed in this phase.

All new tables added in T4 (`usage_events`, `billing_snapshots`) use **bare names** (no `gdpr.` prefix), consistent with `init.sql` and gdpr-core's `write_batch_table` calls.

---

## Module Structure

```
crates/gdpr-billing/src/
├── lib.rs        — public re-exports (including MeteringRecord)
├── meter.rs      — UsageEvent, EventType, NerTier, MeteringRecord, Meter  (replaces skeleton)
├── pricing.rs    — Plan, PlanLimits, PriceSheet, BillingSnapshot, calculate_overage  (NEW)
├── cap.rs        — UsageCap per-plan enforcement  (replaces skeleton)
└── invoice.rs    — Invoice, LineItem, millicent→EUR rendering  (replaces skeleton)
```

`client.rs` deleted — its ClickHouse HTTP logic is replaced by reuse of `gdpr_core::clients::ClickHouseClient`.

`MeteringRecord` lives in `gdpr-billing::meter` and is re-exported from `gdpr-billing::lib`. Handlers import it as `gdpr_billing::MeteringRecord` — clean dependency direction (handler → gdpr-billing, not handler → middleware).

---

## Component Designs

### `meter.rs` — UsageEvent, MeteringRecord, Meter

```rust
pub enum EventType { Ingest, Search, Anonymize, AiChat, Delete }
pub enum NerTier  { L1, L1L2 }

/// Full usage event written to ClickHouse `usage_events` table.
pub struct UsageEvent {
    pub tenant_id:     String,
    pub api_key_id:    String,
    pub request_id:    String,       // from x-request-id header
    pub event_type:    EventType,
    pub document_id:   Option<String>,
    pub chars_in:      u64,
    pub chars_out:     u64,
    pub doc_count:     u32,
    pub chunk_count:   u32,
    pub ai_tokens_in:  u32,
    pub ai_tokens_out: u32,
    pub ner_tier:      NerTier,
    pub latency_ms:    u32,
}

/// Lightweight record inserted into Axum response extensions by each handler.
/// The meter middleware reads this after the response to build the full UsageEvent.
pub struct MeteringRecord {
    pub event_type:    EventType,
    pub document_id:   Option<String>,
    pub chars_in:      u64,
    pub chars_out:     u64,
    pub doc_count:     u32,
    pub ai_tokens_in:  u32,
    pub ai_tokens_out: u32,
    pub ner_tier:      NerTier,
}

pub struct Meter {
    clickhouse: Arc<gdpr_core::clients::ClickHouseClient>,
}

impl Meter {
    pub fn new(clickhouse: Arc<gdpr_core::clients::ClickHouseClient>) -> Self;
    /// Fire-and-forget: spawns a tokio task, never blocks the caller.
    /// On ClickHouse error: logs via tracing::warn!, does not panic.
    pub async fn record(&self, event: UsageEvent);
}
```

`Meter::record` calls `clickhouse.write_batch_table("usage_events", &[event])` inside a `tokio::spawn`. Errors are caught and logged, never propagated.

**`request_id` source:** The meter middleware reads it from the `x-request-id` response header (set by `RequestIdLayer` earlier in the tower stack).

---

### `pricing.rs` — Plan, PriceSheet, BillingSnapshot

`Plan` moves here from `gdpr-api::state`. All callers in gdpr-api update their import to `gdpr_billing::Plan`.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan { Starter, Business, Enterprise }

pub struct PlanLimits {
    pub monthly_docs:      u64,   // included docs
    pub monthly_chars:     u64,   // included chars
    pub monthly_rag:       u64,   // included RAG queries (EventType::Search)
    pub monthly_ai_tokens: u64,   // included AI tokens (in + out combined)
}

pub struct PriceSheet {
    pub per_doc_millicents:        u64,  // 8_000   = €0.08
    pub per_char_per_million:      u64,  // 40_000  = €0.40/1M
    pub per_rag_query_millicents:  u64,  // 4_000   = €0.04
    pub per_token_in_per_million:  u64,  // 150_000 = €1.50/1M
    pub per_token_out_per_million: u64,  // 200_000 = €2.00/1M
    pub l2_surcharge_per_doc:      u64,  // 2_000   = €0.02
}

pub struct BillingSnapshot {
    pub tenant_id:           String,
    pub period_ym:           u32,   // YYYYMM e.g. 202603
    pub total_docs:          u64,
    pub total_chars_in:      u64,
    pub total_rag_queries:   u64,   // count of EventType::Search rows
    pub total_ai_tokens_in:  u64,
    pub total_ai_tokens_out: u64,
}

impl Plan {
    pub fn limits(&self) -> PlanLimits;
    pub fn rate_limit_rpm(&self) -> u32;    // moved from gdpr-api::state
    pub fn max_concurrent(&self) -> usize;  // moved from gdpr-api::state
}

impl Default for PriceSheet {
    fn default() -> Self { /* EU standard rates above */ }
}

impl PriceSheet {
    /// Returns overage charge in EUR millicents. Pure function, no I/O.
    /// Enterprise plan always returns 0.
    pub fn calculate_overage(&self, snapshot: &BillingSnapshot, plan: &Plan) -> u64;
}
```

**`period_ym` construction in Rust:** `chrono::Utc::now().format("%Y%m").to_string().parse::<u32>().unwrap_or(0)`

**Plan limits:**

| Plan       | Docs   | Chars | RAG queries | AI tokens |
|------------|--------|-------|-------------|-----------|
| Starter    | 1,000  | 5M    | 500         | 500K      |
| Business   | 10,000 | 50M   | 5,000       | 5M        |
| Enterprise | ∞      | ∞     | ∞           | ∞         |

`calculate_overage`: for each resource, `max(0, actual - included) * unit_rate`. Sums all resources. Returns `0` for Enterprise.

---

### `cap.rs` — Per-Plan Enforcement

```rust
pub struct UsageCap {
    pub plan: Plan,
}

impl UsageCap {
    /// Returns Err(CapExceeded) if adding this event would breach the monthly plan limit.
    /// Called by meter middleware BEFORE forwarding request — triggers HTTP 429.
    pub fn check(&self, snapshot: &BillingSnapshot, event: &MeteringRecord) -> Result<(), BillingError>;
}

#[derive(Debug, Error)]
pub enum BillingError {
    #[error("usage cap exceeded: {0}")]
    CapExceeded(String),
    #[error("billing error: {0}")]
    Internal(String),   // Note: Internal(String) not anyhow — callers use .map_err(|e| BillingError::Internal(e.to_string()))
}
```

Enterprise plan always returns `Ok(())`.

**Snapshot caching strategy:** `AppState` holds a `Arc<DashMap<String, (BillingSnapshot, Instant)>>` (keyed by `tenant_id`). A background `tokio::spawn` loop refreshes snapshots every 60 seconds by querying `billing_snapshots`. Cap enforcement uses the cached snapshot — best-effort, not guaranteed to the millisecond. This avoids a blocking ClickHouse query on every request.

---

### `invoice.rs` — Millicent-Safe Rendering

```rust
pub struct Invoice {
    pub tenant_id:  String,
    pub period_ym:  u32,
    pub line_items: Vec<LineItem>,
    pub total_eur:  f64,  // f64 only here, at serialization boundary
}

pub struct LineItem {
    pub description:    String,
    pub quantity:       u64,
    pub unit_price_eur: f64,
    pub total_eur:      f64,
}

impl Invoice {
    /// Builds a full invoice from a billing snapshot, plan, and price sheet.
    pub fn from_snapshot(snapshot: &BillingSnapshot, plan: &Plan, sheet: &PriceSheet) -> Self;
}
```

All arithmetic in millicents internally. Converts to `f64` EUR only when constructing `LineItem` fields for JSON output (`millicents as f64 / 100_000.0`).

---

## Cross-Crate Changes

### `gdpr-api/Cargo.toml`
Add:
```toml
gdpr-billing = { path = "../gdpr-billing" }
```

### `gdpr-api/src/state.rs`
- Remove `Plan` enum and its `rate_limit_rpm`/`max_concurrent` methods
- Add `use gdpr_billing::Plan;` (re-export or direct import)
- Add `meter: Arc<gdpr_billing::Meter>` to `AppState`
- Add `snapshot_cache: Arc<DashMap<String, (BillingSnapshot, Instant)>>` to `AppState`

### `gdpr-api/src/middleware/auth.rs`
- Change `use crate::state::Plan` → `use gdpr_billing::Plan`
- Add `plan TEXT NOT NULL DEFAULT 'starter'` to the SQLite `api_keys` SELECT query
- Deserialize `plan` column into `Plan` via `serde_json::from_str` or a match on string; default to `Plan::Starter` if column absent for backwards compatibility

### `gdpr-api/src/middleware/meter.rs`
Replace logging stub:
1. Before calling inner handler: check cap via `UsageCap { plan: auth.plan }.check(&cached_snapshot, &metering_record)` — return 429 if exceeded
2. Call inner handler, measure latency
3. Read `MeteringRecord` from response extensions (inserted by handler)
4. Read `x-request-id` from response headers
5. Build `UsageEvent` from `AuthContext + MeteringRecord + request_id + latency_ms`
6. Call `state.meter.record(event)` — fire-and-forget

### `gdpr-api/src/handlers/usage.rs`
Replace stub:
```rust
pub async fn get_usage(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<Invoice>> {
    let period_ym = current_period_ym();
    let snapshot = query_billing_snapshot(&state.clickhouse, &auth.tenant_id, period_ym).await?;
    let invoice = Invoice::from_snapshot(&snapshot, &auth.plan, &PriceSheet::default());
    Ok(Json(invoice))
}
```

### `gdpr-api/src/main.rs`
Construct billing types at startup:
```rust
let core_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new(&clickhouse_url));
let meter   = Arc::new(gdpr_billing::Meter::new(core_ch));
```

### `config/clickhouse/init.sql`
Add at end of file:

```sql
-- ── Extend gdpr_audit with multi-tenant correlation fields ────────────────────
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS tenant_id  String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS request_id String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS api_key_id String DEFAULT '';

-- ── Usage events (billing metering, high-volume append-only) ──────────────────
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
    ner_tier      Enum8('l1'=1, 'l1_l2'=2),
    latency_ms    UInt32   DEFAULT 0,
    ts            DateTime DEFAULT now()
) ENGINE = MergeTree()
  PARTITION BY toYYYYMM(ts)
  ORDER BY (tenant_id, ts)
  SETTINGS index_granularity = 8192;

-- ── Billing snapshots (materialized view target) ──────────────────────────────
CREATE TABLE IF NOT EXISTS billing_snapshots (
    tenant_id            String,
    period_ym            UInt32,
    total_docs           UInt64,
    total_chars_in       UInt64,
    total_rag_queries    UInt64,
    total_ai_tokens_in   UInt64,
    total_ai_tokens_out  UInt64,
    computed_at          DateTime DEFAULT now()
) ENGINE = ReplacingMergeTree(computed_at)
  ORDER BY (tenant_id, period_ym);

CREATE MATERIALIZED VIEW IF NOT EXISTS mv_billing_snapshots
TO billing_snapshots AS
SELECT
    tenant_id,
    toYYYYMM(ts)                                AS period_ym,
    countIf(event_type = 'ingest')              AS total_docs,
    sum(chars_in)                               AS total_chars_in,
    countIf(event_type = 'search')              AS total_rag_queries,
    sum(toUInt64(ai_tokens_in))                 AS total_ai_tokens_in,
    sum(toUInt64(ai_tokens_out))                AS total_ai_tokens_out,
    now()                                       AS computed_at
FROM usage_events
GROUP BY tenant_id, toYYYYMM(ts);
```

---

## Data Flow

```
HTTP Request
    │
    ▼
AuthLayer → injects AuthContext (tenant_id, api_key_id, Plan) into extensions
    │
    ▼
MeterLayer (pre) → UsageCap::check(cached_snapshot, estimated_event) → 429 if exceeded
    │
    ▼
Handler → processes request, inserts MeteringRecord into response extensions
    │
    ▼
MeterLayer (post) → reads AuthContext + MeteringRecord + x-request-id + latency
                  → builds UsageEvent
                  → state.meter.record(event)   [tokio::spawn, never blocks]
    │                         │
    │                         ▼
    │               gdpr_core ClickHouseClient → usage_events table
    ▼
Response returned to caller
```

---

## Error Handling

- `Meter::record` — ClickHouse errors logged via `tracing::warn!`, never propagated; includes test case for down-path (H4 test gap resolved)
- `UsageCap::check` — returns `BillingError::CapExceeded` → middleware maps to HTTP 429 with `Retry-After: 60` header
- `Invoice::from_snapshot` — pure function, no errors; returns zero-cost invoice if snapshot is empty
- `BillingError::Internal(String)` — callers use `.map_err(|e| BillingError::Internal(e.to_string()))`, no `#[from] anyhow::Error`

---

## Testing

```bash
# Unit tests (pure functions, no I/O)
cargo test -p gdpr-billing -- pricing        # overage calculation with known inputs
cargo test -p gdpr-billing -- cap            # cap enforcement per plan
cargo test -p gdpr-billing -- invoice        # millicent → EUR rendering

# Integration tests
cargo test -p gdpr-api -- middleware::meter  # MeteringRecord → UsageEvent emission
cargo test -p gdpr-api -- handlers::usage    # usage handler returns real snapshot data
cargo test -p gdpr-api -- middleware::auth   # plan column deserialization
```

Key unit test cases:
- Starter plan: 999 docs included → overage = 0; 1001 docs → overage = 1 × 8_000 millicents
- Enterprise plan: any usage → overage = 0
- Millicent arithmetic: `1_234_567 millicents → 12.34567 EUR` (no rounding loss)
- Cap check: Starter at 1000 docs + 1 more ingest event → `CapExceeded`
- `Meter::record` with ClickHouse down: call completes without panic, `tracing::warn!` emitted

---

## Files Changed

| File | Action |
|------|--------|
| `hacienda/crates/gdpr-billing/src/meter.rs` | Replace — add UsageEvent, MeteringRecord, Meter |
| `hacienda/crates/gdpr-billing/src/pricing.rs` | Create — Plan, PriceSheet, BillingSnapshot |
| `hacienda/crates/gdpr-billing/src/cap.rs` | Replace — plan-aware cap enforcement |
| `hacienda/crates/gdpr-billing/src/invoice.rs` | Replace — millicent arithmetic |
| `hacienda/crates/gdpr-billing/src/client.rs` | Delete |
| `hacienda/crates/gdpr-billing/src/lib.rs` | Update re-exports |
| `hacienda/crates/gdpr-api/Cargo.toml` | Add gdpr-billing dep |
| `hacienda/crates/gdpr-api/src/state.rs` | Remove Plan, add meter + snapshot_cache fields |
| `hacienda/crates/gdpr-api/src/main.rs` | Construct gdpr_core ClickHouseClient + Meter |
| `hacienda/crates/gdpr-api/src/middleware/auth.rs` | Fix Plan import + plan column from DB |
| `hacienda/crates/gdpr-api/src/middleware/meter.rs` | Replace stub with real cap check + UsageEvent emission |
| `hacienda/crates/gdpr-api/src/handlers/usage.rs` | Replace stub with snapshot query + Invoice |
| `hacienda/config/clickhouse/init.sql` | Add usage_events, billing_snapshots, MV, ALTER gdpr_audit |
