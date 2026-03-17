# T4 — gdpr-billing Crate: Full Rewrite Design

**Date:** 2026-03-17
**Status:** Approved
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

## Module Structure

```
crates/gdpr-billing/src/
├── lib.rs        — public re-exports
├── meter.rs      — UsageEvent, EventType, NerTier, Meter  (replaces skeleton)
├── pricing.rs    — Plan, PlanLimits, PriceSheet, BillingSnapshot, calculate_overage  (NEW)
├── cap.rs        — UsageCap per-plan enforcement  (replaces skeleton)
└── invoice.rs    — Invoice, LineItem, millicent→EUR rendering  (replaces skeleton)
```

`client.rs` deleted — its ClickHouse HTTP logic is replaced by reuse of `gdpr_core::clients::ClickHouseClient`.

---

## Component Designs

### `meter.rs` — UsageEvent + Meter

```rust
pub enum EventType { Ingest, Search, Anonymize, AiChat, Delete }
pub enum NerTier  { L1, L1L2 }

pub struct UsageEvent {
    pub tenant_id:     String,
    pub api_key_id:    String,
    pub request_id:    String,
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

pub struct Meter {
    clickhouse: Arc<gdpr_core::clients::ClickHouseClient>,
}

impl Meter {
    pub fn new(clickhouse: Arc<gdpr_core::clients::ClickHouseClient>) -> Self;
    pub async fn record(&self, event: UsageEvent);  // fire-and-forget, never blocks response
}
```

`Meter::record` serializes `UsageEvent` as JSON and inserts into `gdpr.usage_events` via `ClickHouseClient`. Uses `tokio::spawn` internally — caller is never blocked.

---

### `pricing.rs` — Plan, PriceSheet, BillingSnapshot

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan { Starter, Business, Enterprise }

pub struct PlanLimits {
    pub monthly_docs:      u64,   // included docs
    pub monthly_chars:     u64,   // included chars
    pub monthly_rag:       u64,   // included RAG queries
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
    pub period_ym:           u32,   // YYYYMM
    pub total_docs:          u64,
    pub total_chars_in:      u64,
    pub total_rag_queries:   u64,
    pub total_ai_tokens_in:  u64,
    pub total_ai_tokens_out: u64,
}

impl Plan {
    pub fn limits(&self) -> PlanLimits;
    pub fn rate_limit_rpm(&self) -> u32;    // moved from gdpr-api::state
    pub fn max_concurrent(&self) -> usize;  // moved from gdpr-api::state
}

impl PriceSheet {
    pub fn default() -> Self;
    /// Returns overage charge in EUR millicents. Pure function, no I/O.
    pub fn calculate_overage(&self, snapshot: &BillingSnapshot, plan: &Plan) -> u64;
}
```

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
pub struct UsageCap { pub plan: Plan }

impl UsageCap {
    /// Returns Err(CapExceeded) if adding this event would breach the monthly plan limit.
    /// Called by meter middleware before recording — triggers HTTP 429.
    pub fn check(&self, snapshot: &BillingSnapshot, event: &UsageEvent) -> Result<(), BillingError>;
}

#[derive(Debug, Error)]
pub enum BillingError {
    #[error("usage cap exceeded: {0}")]
    CapExceeded(String),
    #[error("billing error: {0}")]
    Internal(String),
}
```

Checks docs, chars, RAG queries, and AI tokens independently against `plan.limits()`. Enterprise plan always returns `Ok(())`.

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

All arithmetic in millicents internally. Converts to `f64` EUR only when constructing `LineItem` fields for JSON output.

---

## Cross-Crate Changes

### `gdpr-api/Cargo.toml`
Add:
```toml
gdpr-billing = { path = "../gdpr-billing" }
```

### `gdpr-api/src/state.rs`
- Remove `Plan` enum and its `rate_limit_rpm`/`max_concurrent` methods
- Import `Plan` from `gdpr_billing::pricing`
- Add `meter: Arc<gdpr_billing::Meter>` to `AppState`

### `gdpr-api/src/middleware/auth.rs`
- Import `Plan` from `gdpr_billing::pricing` instead of `crate::state`
- Add `plan TEXT NOT NULL DEFAULT 'starter'` to SQLite `api_keys` SELECT
- Deserialize `plan` column into `Plan` enum (default `Plan::Starter` if column absent)

### `gdpr-api/src/middleware/meter.rs`
Replace logging stub with real emission:
1. After inner handler returns, read `AuthContext` from request extensions
2. Read `MeteringRecord` from response extensions (populated by handlers)
3. Build `UsageEvent` from auth context + metering record + latency
4. Call `state.meter.record(event)` — fire-and-forget

`MeteringRecord` is a lightweight struct inserted into response extensions by each handler:
```rust
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
```

### `gdpr-api/src/handlers/usage.rs`
Replace stub:
- Query `gdpr.billing_snapshots` from ClickHouse for current tenant + current month
- Build `Invoice` via `Invoice::from_snapshot(&snapshot, &auth.plan, &PriceSheet::default())`
- Return JSON invoice

### `config/clickhouse/init.sql`
Add:
```sql
-- Extend gdpr_audit with correlation fields
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS tenant_id  String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS request_id String DEFAULT '';
ALTER TABLE gdpr_audit ADD COLUMN IF NOT EXISTS api_key_id String DEFAULT '';

-- Usage events (billing metering)
CREATE TABLE IF NOT EXISTS gdpr.usage_events ( ... );  -- full spec schema

-- Billing snapshots + materialized view
CREATE TABLE IF NOT EXISTS gdpr.billing_snapshots ( ... );
CREATE MATERIALIZED VIEW IF NOT EXISTS gdpr.mv_billing_snapshots
TO gdpr.billing_snapshots AS SELECT ... FROM gdpr.usage_events GROUP BY tenant_id, toYYYYMM(ts);
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
Handler → processes request, inserts MeteringRecord into response extensions
    │
    ▼
MeterLayer → reads AuthContext + MeteringRecord, calls Meter::record(UsageEvent)
    │                                                         │
    │                                                         ▼ (fire-and-forget)
    │                                               ClickHouseClient → gdpr.usage_events
    ▼
Response returned to caller (never blocked by metering)
```

---

## Error Handling

- `Meter::record` — errors logged via `tracing::warn!`, never propagated to caller
- `UsageCap::check` — returns `BillingError::CapExceeded` → middleware maps to HTTP 429 with `Retry-After` header
- `Invoice::from_snapshot` — pure function, no errors; if snapshot missing returns zero invoice

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
- Starter plan: 999 docs included → overage = 0; 1001 docs → overage = 1 × €0.08
- Enterprise plan: any usage → overage = 0
- Millicent arithmetic: `1_234_567 millicents → €12.34567` (no rounding loss)
- Cap check: Starter at 1000 docs + 1 more → `CapExceeded`

---

## Files Changed

| File | Action |
|------|--------|
| `hacienda/crates/gdpr-billing/src/meter.rs` | Replace |
| `hacienda/crates/gdpr-billing/src/pricing.rs` | Create |
| `hacienda/crates/gdpr-billing/src/cap.rs` | Replace |
| `hacienda/crates/gdpr-billing/src/invoice.rs` | Replace |
| `hacienda/crates/gdpr-billing/src/client.rs` | Delete |
| `hacienda/crates/gdpr-billing/src/lib.rs` | Update re-exports |
| `hacienda/crates/gdpr-api/Cargo.toml` | Add gdpr-billing dep |
| `hacienda/crates/gdpr-api/src/state.rs` | Remove Plan, add meter field |
| `hacienda/crates/gdpr-api/src/middleware/auth.rs` | Fix Plan import + plan column |
| `hacienda/crates/gdpr-api/src/middleware/meter.rs` | Replace stub with real emission |
| `hacienda/crates/gdpr-api/src/handlers/usage.rs` | Replace stub |
| `hacienda/config/clickhouse/init.sql` | Add usage_events, billing_snapshots, MV, ALTERs |
