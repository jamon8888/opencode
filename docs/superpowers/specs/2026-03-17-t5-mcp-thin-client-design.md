# T5 — gdpr-mcp Thin Client Design

## Goal

Rewrite `gdpr-mcp` from a fat in-process monolith (owns PII engine, NER, SQLite, kreuzberg) into a thin HTTP client that delegates all business logic to `gdpr-api` over REST. gdpr-mcp becomes a pure protocol adapter: MCP stdio ↔ HTTP.

## Context

`gdpr-mcp` currently embeds `gdpr-core` as a compile-time dependency, running ML models (GLiNER ONNX), SQLite, AES-256-GCM vault, and kreuzberg document extraction in-process. This duplicates everything `gdpr-api` already provides. The binary is ~200MB and requires ONNX/cmake at build time.

After T5 the binary carries only `rmcp` (MCP protocol), `reqwest` (HTTP), `serde_json`, and `tokio`. Binary drops to ~5MB. All capability lives in `gdpr-api`.

**Workspace note:** The canonical `gdpr-mcp` is at `hacienda/crates/gdpr-mcp/` (workspace member). The standalone version at `opencode/gdpr-mcp/` is the old monolith and is not touched by T5.

## Architecture

### Before T5

```
AI Agent → gdpr-mcp (MCP stdio)
                ↓
          gdpr-core (in-process)
          SQLite / NER / PII / kreuzberg
```

### After T5

```
AI Agent → gdpr-mcp (MCP stdio)
                ↓  HTTP + Bearer token
          gdpr-api (REST, port 3001)
                ↓
          gdpr-core (in-process, owned by api)
          SQLite / NER / PII / kreuzberg
```

The anonymization proxy (`POST :8080/openai/v1/chat/completions`) stays in gdpr-mcp but replaces `engine_pool.anonymize_batch()` with `POST /v1/anonymize` and `POST /v1/deanonymize` calls. The proxy supplies `legal_basis: "legitimate_interest"` for all anonymize calls (chat messages have no other applicable legal basis in the GDPR sense).

## Configuration

Two environment variables read at startup. Missing either → panic with clear message, do not start.

| Variable | Example | Purpose |
|----------|---------|---------|
| `GDPR_API_URL` | `http://gdpr-api:3001` | Base URL of gdpr-api |
| `GDPR_API_KEY` | `gdpr_key_abc123` | Bearer token for all requests |

**Remove** the existing `CLOAKPIPE_VAULT_KEY` startup check from `main.rs` — the vault is owned by gdpr-api after T5.

## Two-Part Implementation

### Part 1 — Complete gdpr-api Stub Handlers

Five gdpr-api handlers are currently stubs and must be real before gdpr-mcp can delegate to them.

#### `POST /v1/documents` — Document Ingest

**Request:**
```json
{ "text": "...", "legal_basis": "consent", "profile": "default" }
```

**`file_path` input:** Not supported in T5. If the caller supplies a `file_path` field instead of `text`, the API returns HTTP 400 with `detail: "file_path upload not supported; provide text directly"`. The MCP thin client propagates this as a tool error without special-casing it.

**Change `IngestReq.legal_basis` to required:** The existing stub has `legal_basis: Option<String>`. Change it to `legal_basis: String` (non-optional) to match the anonymize handler's requirement. Add `#[validate(length(min = 1))]` attribute consistent with other request structs. If the caller omits `legal_basis`, the API returns 400 from the validator before the handler body runs.

**Implementation:**
1. Use the `text` field directly (no kreuzberg for T5 — text-only ingest)
2. Anonymize using the same code path as the existing `POST /v1/anonymize` handler (`handlers/anonymize.rs`). Follow this pattern exactly:
   ```rust
   use gdpr_core::pii::{AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool};

   let session_id = uuid::Uuid::new_v4().to_string();
   let mut session_ctx = SessionContext::new(profile);          // takes AnonProfile only
   let pool_strings: Vec<String> = get_pool(&profile).iter().map(|s| s.to_string()).collect();
   let engine = TreatmentEngine::new(pool_strings);             // takes Vec<String>
   let result = tokio::task::spawn_blocking(move || {
       anonymize_with_profile(&text, profile, &mut session_ctx, &engine)
   }).await??;
   // Store token map in session_cache so deanonymize works for this doc
   {
       let dm = dashmap::DashMap::new();
       for (token, original) in &result.token_map {
           dm.insert(token.clone(), original.clone());
       }
       state.session_cache.insert(session_id.clone(), crate::state::SessionCache {
           token_map: dm, created_at: std::time::Instant::now(),
       });
   }
   ```
3. Chunk with `chunk_text` from gdpr-core using **400-word chunks with 50-word overlap** (matching existing MCP server constants)
4. Store in SQLite using `state.db`. **First, ensure the document storage tables exist** — add these migrations to `gdpr-api/src/main.rs` (alongside `api_keys` and `usage_records`):
   ```sql
   CREATE TABLE IF NOT EXISTS documents (
       id TEXT PRIMARY KEY, anon_text TEXT NOT NULL, pii_count INTEGER NOT NULL,
       ner_degraded INTEGER NOT NULL, created_at INTEGER NOT NULL
   );
   CREATE TABLE IF NOT EXISTS doc_chunks (
       id TEXT PRIMARY KEY, doc_id TEXT NOT NULL, chunk_idx INTEGER NOT NULL,
       chunk_text TEXT NOT NULL, chunk_offset INTEGER NOT NULL, created_at INTEGER NOT NULL
   );
   CREATE TABLE IF NOT EXISTS doc_entity_map (
       id INTEGER PRIMARY KEY AUTOINCREMENT,
       document_id TEXT NOT NULL, entity_type TEXT NOT NULL,
       pseudonym TEXT NOT NULL, detection_layer TEXT NOT NULL,
       confidence REAL, ner_degraded INTEGER NOT NULL DEFAULT 0,
       created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
   );
   ```
   Then insert:
   - `INSERT INTO documents (id, anon_text, pii_count, ner_degraded, created_at) VALUES (?, ?, ?, ?, unixepoch())`
   - `INSERT INTO doc_chunks (id, doc_id, chunk_idx, chunk_text, chunk_offset, created_at) VALUES (?, ?, ?, ?, ?, unixepoch())` per chunk
   - `INSERT INTO doc_entity_map (document_id, entity_type, pseudonym, detection_layer, confidence, ner_degraded) VALUES (?, ?, ?, ?, ?, ?)` per entity

   No `DocAudit` helper exists in `AppState`; use `state.db.get().await?.interact(|c| { ... })` directly.
5. Write ClickHouse audit trail
6. Return full response

**Extend `IngestResp` struct** in `documents.rs` to include `chunk_count: usize` and `decision_explanation: String` before implementing the handler body. The existing struct only has `doc_id`, `pii_count`, `ner_degraded`, `ai_act_risk_level`.

**Response:**
```json
{
  "doc_id": "uuid",
  "pii_count": 12,
  "ner_degraded": false,
  "ai_act_risk_level": "medium",
  "chunk_count": 5,
  "decision_explanation": "..."
}
```

#### `GET /v1/documents` — List Documents

**Implementation:** Query SQLite `documents` table joined with `doc_entity_map`, return id, created_at, entity count per doc.

**Struct changes:** The existing `DocList` struct uses field name `items` and a `total` count. Rename `items` to `documents`, drop `total`, and use a typed element struct:
```rust
#[derive(Serialize)]
pub struct DocEntry {
    pub doc_id: String,
    pub created_at: i64,   // Unix timestamp seconds, matching SQLite schema
    pub entity_count: usize,
}

#[derive(Serialize)]
pub struct DocList {
    pub documents: Vec<DocEntry>,
}
```

**SQL query:** The `documents` table uses `id` as the primary key column, not `doc_id`. Use an alias:
```sql
SELECT d.id AS doc_id, d.created_at, COUNT(e.document_id) AS entity_count
FROM documents d
LEFT JOIN doc_entity_map e ON d.id = e.document_id
GROUP BY d.id
ORDER BY d.created_at DESC
```

**Response:**
```json
{ "documents": [{ "doc_id": "uuid", "created_at": 1710000000, "entity_count": 12 }] }
```

#### `DELETE /v1/documents/:id` — Delete Document

**Implementation:**
1. Delete from `doc_chunks`, `doc_entity_map`, `documents` in SQLite
2. Delete embeddings from VecStore (if enabled)
3. Write ClickHouse audit trail
4. Return 204 on success, 404 if doc not found

#### `POST /v1/search` — Semantic/Keyword Search

**Request:**
```json
{ "query": "...", "limit": 10, "profile": "default" }
```

**Implementation:**
1. Try VecStore semantic search if `state.vec_store` is `Some`
2. Fall back to SQLite `LIKE` query on `doc_chunks.chunk_text` (the actual column name in the DDL)
3. Return top-N matching chunks with doc_id and score

**Response:**
```json
{ "results": [{ "doc_id": "uuid", "chunk": "...", "score": 0.92 }] }
```

#### `GET /v1/review-queue` — High-PII Document Queue

**Query parameters:**
- `threshold` (optional, default 20): minimum entity count to include
- `limit` (optional, default 50): maximum documents to return

**Implementation:** Query `doc_entity_map` grouped by `doc_id`, joined with `documents` for `created_at`, filter by entity count ≥ threshold, order descending by entity count, return up to limit.

**ReviewQueueQuery struct** (add to `audit.rs`):
```rust
#[derive(Deserialize)]
pub struct ReviewQueueQuery {
    pub threshold: Option<u32>,
    pub limit:     Option<u32>,
}
```
The handler signature becomes: `pub async fn get_review_queue(State(state): State<AppState>, Query(params): Query<ReviewQueueQuery>) -> ApiResult<Json<ReviewQueueResp>>`

**Struct changes:** The existing stub returns `{"items": [], "total": 0}`. Replace with a typed response:
```rust
#[derive(Serialize)]
pub struct ReviewEntry {
    pub doc_id: String,
    pub entity_count: usize,
    pub created_at: i64,   // Unix timestamp seconds
}

#[derive(Serialize)]
pub struct ReviewQueueResp {
    pub documents: Vec<ReviewEntry>,
}
```
The field name changes from `items` → `documents` and `total` is dropped.

**Response:**
```json
{ "documents": [{ "doc_id": "uuid", "entity_count": 47, "created_at": 1710000000 }] }
```

---

### Part 2 — gdpr-mcp Thin Client Rewrite

#### New: `crates/gdpr-mcp/src/api_client.rs`

Single struct wrapping `reqwest::Client` with a 30-second request timeout. One method per gdpr-api endpoint.

```rust
pub struct ApiClient {
    client:   reqwest::Client,  // configured with 30s timeout
    base_url: String,
    api_key:  String,
}

impl ApiClient {
    pub fn from_env() -> Self {
        // reads GDPR_API_URL + GDPR_API_KEY
        // panics with clear message if either is missing
    }

    pub async fn anonymize(&self, req: AnonymizeRequest) -> Result<AnonymizeResponse, ApiError>;
    pub async fn deanonymize(&self, req: DeanonymizeRequest) -> Result<DeanonymizeResponse, ApiError>;
    pub async fn ingest_document(&self, req: IngestRequest) -> Result<IngestResponse, ApiError>;
    pub async fn list_documents(&self) -> Result<ListDocumentsResponse, ApiError>;
    pub async fn delete_document(&self, doc_id: &str) -> Result<(), ApiError>;
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, ApiError>;
    pub async fn get_audit(&self, limit: u32) -> Result<AuditResponse, ApiError>;
    // NOTE: doc_id filtering on audit is deferred to T6+. The gdpr-api GET /v1/audit endpoint
    // accepts only `limit` and `action` query params; no doc_id filter exists yet.
    pub async fn get_review_queue(&self, threshold: u32, limit: u32) -> Result<ReviewQueueResponse, ApiError>;
}
```

**Defaults resolved in MCP tool handlers before calling ApiClient:**
- `get_audit`: `limit` defaults to 50 in the tool handler; `doc_id` parameter is accepted by the MCP tool but **not forwarded** (T6+ work) — the handler calls `api_client.get_audit(limit.unwrap_or(50))`
- `get_review_queue`: `threshold` defaults to 20, `limit` defaults to 50 in the tool handler

All methods add `Authorization: Bearer <api_key>` header. All serialize request body as JSON, deserialize response as JSON.

#### Type Definitions

All request/response types live in `api_client.rs`. Field names must match gdpr-api's JSON serialization exactly.

```rust
// --- Anonymize ---
#[derive(Serialize, Default)]
pub struct AnonymizeRequest {
    pub text: String,
    pub legal_basis: String,          // required by gdpr-api
    pub profile: Option<String>,
    pub session_id: Option<String>,
}
#[derive(Deserialize)]
pub struct AnonymizeResponse {
    pub anonymized_text: String,
    pub session_id: String,
    pub pii_count: usize,             // actual field in gdpr-api AnonymizeResponse
}

// --- Deanonymize ---
#[derive(Serialize, Default)]
pub struct DeanonymizeRequest {
    pub text: String,
    pub session_id: Option<String>,
}
#[derive(Deserialize)]
pub struct DeanonymizeResponse {
    pub text: String,
}

// --- Ingest ---
#[derive(Serialize)]
pub struct IngestRequest {
    pub text: String,
    pub legal_basis: String,
    pub profile: Option<String>,
}
#[derive(Deserialize)]
pub struct IngestResponse {
    pub doc_id: String,
    pub pii_count: usize,
    pub ner_degraded: bool,
    pub ai_act_risk_level: String,
    pub chunk_count: usize,
    pub decision_explanation: String,
}

// --- List Documents ---
#[derive(Deserialize)]
pub struct DocEntry {
    pub doc_id: String,
    pub created_at: i64,
    pub entity_count: usize,
}
#[derive(Deserialize)]
pub struct ListDocumentsResponse {
    pub documents: Vec<DocEntry>,
}

// --- Search ---
#[derive(Serialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<u32>,
    pub profile: Option<String>,
}
#[derive(Deserialize)]
pub struct SearchResult {
    pub doc_id: String,
    pub chunk: String,
    pub score: f64,
}
#[derive(Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

// --- Audit ---
// Field names match actual gdpr-api AuditEvent struct (audit.rs) exactly.
// All 12 fields included — GDPR compliance fields (legal_basis, ai_act_risk_level,
// decision_explanation) must be surfaced to AI agents for audit purposes.
#[derive(Deserialize)]
pub struct AuditEntry {
    pub document_id:          String,
    pub action:               String,
    pub pii_count_before:     u32,
    pub pii_count_after:      u32,
    pub ner_degraded:         u8,
    pub processing_time_ms:   u32,
    pub legal_basis:          String,
    pub user_id:              String,
    pub model_version:        String,
    pub ai_act_risk_level:    String,
    pub decision_explanation: String,
    pub ts_unix:              u64,
}
#[derive(Deserialize)]
pub struct AuditResponse {
    pub events: Vec<AuditEntry>,  // top-level key is "events" in gdpr-api response
}

// --- Review Queue ---
#[derive(Deserialize)]
pub struct ReviewEntry {
    pub doc_id: String,
    pub entity_count: usize,
    pub created_at: i64,
}
#[derive(Deserialize)]
pub struct ReviewQueueResponse {
    pub documents: Vec<ReviewEntry>,
}
```

#### `ApiError`

```rust
pub enum ApiError {
    Http { status: u16, detail: String },   // 4xx/5xx from gdpr-api
    Network(String),                         // reqwest error (including timeout)
}

impl std::fmt::Display for ApiError {
    // "gdpr-api error 429: usage cap exceeded: ..."
    // "gdpr-api unavailable: connection refused"
    // "gdpr-api unavailable: request timed out after 30s"
}
```

**Timeout detection:** When converting a `reqwest::Error` to `ApiError::Network`, call `e.is_timeout()` to distinguish timeout from other network errors:
```rust
if e.is_timeout() {
    ApiError::Network("request timed out after 30s".to_string())
} else {
    ApiError::Network(e.to_string())
}
```
This produces the exact display string `"gdpr-api unavailable: request timed out after 30s"` tested by the ApiClient tests.

MCP tool handlers convert `ApiError` to an MCP error string. 404 from `delete_document` surfaces as `"Document not found: <id>"` to preserve the existing MCP wire behavior.

#### Modified: `crates/gdpr-mcp/src/mcp/tools.rs`

Each of the 8 tool handlers becomes a thin wrapper:

| MCP Tool | ApiClient call | Notes |
|----------|---------------|-------|
| `gdpr_ingest` | `api_client.ingest_document(...)` | `file_path` input → tool error (400 from API) |
| `gdpr_search` | `api_client.search(...)` | |
| `gdpr_audit` | `api_client.get_audit(limit.unwrap_or(50))` | default resolved here; doc_id param accepted but not forwarded (T6+) |
| `gdpr_delete` | `api_client.delete_document(doc_id)` | 404 → "Document not found: {id}" |
| `gdpr_anonymize` | `api_client.anonymize(...)` | |
| `gdpr_deanonymize` | `api_client.deanonymize(...)` | |
| `gdpr_list_documents` | `api_client.list_documents()` | |
| `gdpr_review_queue` | `api_client.get_review_queue(threshold.unwrap_or(20), limit.unwrap_or(50))` | defaults resolved here |

Tool parameter parsing (MCP JSON → typed struct) and result serialization (typed struct → MCP JSON) remain in the tool handlers. Business logic moves entirely to gdpr-api.

#### Modified: `crates/gdpr-mcp/src/proxy/mod.rs`

Replace:
```rust
engine_pool.anonymize_batch(slots).await
// ...
engine_pool.deanonymize(tokens).await
```

With:
```rust
let anon_resp = api_client.anonymize(AnonymizeRequest {
    text: joined_slots,
    legal_basis: "legitimate_interest".to_string(),
    ..Default::default()
}).await?;
let session_id = anon_resp.session_id.clone(); // thread through to deanonymize

// ... forward to TensorZero using anon_resp.anonymized_text ...

api_client.deanonymize(DeanonymizeRequest {
    text: rehydrate_input,
    session_id: Some(session_id),
    ..Default::default()
}).await
```

**Session ID threading:** The gdpr-api anonymize endpoint returns a `session_id` in the response. The proxy must store this `session_id` as a local variable and pass it as `DeanonymizeRequest.session_id`. Without it, gdpr-api cannot look up the token map and deanonymization returns the text unchanged (silent data corruption). The session_id is a local variable within the single async request handler — no shared state is needed.

**SSE streaming path after T5:** The current proxy uses an in-memory local token map for per-chunk token replacement during SSE streaming. After T5 this local map is gone. The new behavior:
1. The proxy anonymizes the request text (unchanged — happens before TensorZero forward)
2. The proxy forwards to TensorZero and **buffers the complete SSE response** (collect all `data:` events into a single string)
3. The proxy calls `api_client.deanonymize` once on the buffered text with `session_id` from step 1
4. The proxy returns the deanonymized result as a single non-streaming HTTP response to the chat client

This means after T5 the proxy **no longer streams responses back** — it buffers and returns. The TensorZero-side SSE streaming is still used (to get the full response without timeout), but the client-facing response is non-streaming. This is a deliberate T5 trade-off; streaming passthrough with API-side deanonymization is deferred to T6+.

The existing `rehydrate_from_cache` function and `write_text_slots` usage in the SSE path are replaced by the single `api_client.deanonymize` call after buffering.

**Remove `ProxyState.session_cache`:** The existing `DashMap`-backed session cache in `ProxyState` stored in-process token maps. After T5, token maps are owned by gdpr-api. The `ProxyState.session_cache` field and the `SessionCache` struct are **removed**. `dashmap` is no longer needed in gdpr-mcp.

**`uuid` removal is correct:** The proxy previously generated session UUIDs locally (`uuid::Uuid::new_v4()`). After T5 the session_id comes from gdpr-api's anonymize response. No new UUIDs are generated in gdpr-mcp, so `uuid` can be removed.

The proxy's HTTP server on `:8080`, the slot collection/rehydration logic (`collect_text_slots`, `write_text_slots`), and TensorZero forwarding are **unchanged**. Only the anonymization internals are replaced.

#### Modified: `crates/gdpr-mcp/src/main.rs`

- Remove `CoreState` construction (engine pool loading, SQLite open, VecStore init)
- Remove `CLOAKPIPE_VAULT_KEY` startup check
- Remove the `session_cache` local variable (`Arc<DashMap<...>>`)
- Remove the session GC `tokio::spawn` block that references `session_cache` and `proxy::SessionCache`
- Remove the `session_cache: Arc::clone(&session_cache)` field from `ProxyState` construction
- Add: `let api_client = Arc::new(ApiClient::from_env());`
- Pass `api_client` into both `McpState` and `ProxyState`

#### Modified: `crates/gdpr-mcp/Cargo.toml`

**Remove:**
- `gdpr-core`
- `cloakpipe-core`
- `gline-rs`
- `kreuzberg`
- `rusqlite` / `deadpool-sqlite`
- `unicode-segmentation` (used only by NER chunking path)
- `num_cpus` (used only to size EnginePool)
- `prometheus` (used only by in-process engine metrics)
- `uuid` (doc IDs now generated by gdpr-api)

**Keep:**
- `rmcp` (MCP protocol)
- `reqwest` (HTTP client)
- `tokio`, `serde`, `serde_json`, `tracing`
- `axum` (proxy HTTP server on :8080)
- `schemars` (MCP param struct JSON schemas — `#[derive(JsonSchema)]` stays)
- `futures-util` (proxy SSE streaming: `StreamExt` in `proxy/mod.rs`)
- `tokio-stream` (proxy SSE streaming: event streaming in `proxy/mod.rs`)
- `regex` (proxy token rehydration: `TOKEN_RE` pattern in `proxy/mod.rs`)

**Keep (already present):**
- `anyhow` (used by `main.rs` — do NOT remove; not in the Remove list above but must be explicitly preserved)

**Add (not currently in `gdpr-mcp/Cargo.toml`):**
- `thiserror` (new dep, needed for `ApiError` — was not previously in gdpr-mcp; add as workspace dep or `thiserror = "2"`)

**Add to `[dev-dependencies]`:**
- `wiremock = "0.6"` (mock HTTP server for ApiClient unit tests)

## Error Handling

| Scenario | Behavior |
|----------|----------|
| `GDPR_API_URL` or `GDPR_API_KEY` missing at startup | panic: `"T5: GDPR_API_URL must be set"` |
| gdpr-api returns 4xx | MCP tool error with `detail` field from API response |
| gdpr-api returns 404 on delete | MCP tool error: `"Document not found: <id>"` |
| gdpr-api returns 5xx or network failure | MCP tool error: `"gdpr-api unavailable: <reason>"` where `<reason>` is the `detail` field from the ProblemDetail body (e.g. `"An internal error occurred"` — gdpr-api intentionally scrubs internal details from 5xx; full reason is in gdpr-api server logs) |
| reqwest timeout (30s) | MCP tool error: `"gdpr-api unavailable: request timed out after 30s"` |
| Proxy anonymize/deanonymize call fails | HTTP 502 to chat client |
| `gdpr_ingest` with `file_path` | Tool error: `"file_path upload not supported; provide text directly"` |

No retries in gdpr-mcp — callers (AI agents, chat clients) own retry logic.

## Testing

### gdpr-api stub handlers (Part 1)
- TDD: write failing tests first using real SQLite (existing pattern)
- Each handler tested via `axum::test` helpers
- `post_document`: verify doc stored, pii_count + chunk_count + decision_explanation returned, audit written
- `delete_document`: verify 204 on success, 404 on missing doc, VecStore deletion called
- `list_documents`: verify returns all docs with entity counts
- `post_search`: verify chunk results returned for matching query
- `get_review_queue`: verify ordering by entity count, threshold and limit respected

### ApiClient (Part 2)
- Unit tests using `wiremock` to mock gdpr-api responses
- Test each method: happy path, 4xx error, 5xx error, timeout
- Verify `Authorization: Bearer` header always sent
- Verify `delete_document` 404 surfaces as correct error variant

### MCP tools
- Integration tests: existing tool test structure preserved; only handler bodies change
- `gdpr_audit` with absent `limit` → verify default 50 sent to API
- `gdpr_review_queue` with absent `threshold`/`limit` → verify defaults 20/50 sent

## File Map

| File | Action | Notes |
|------|--------|-------|
| `crates/gdpr-api/src/main.rs` | Add migrations | Add documents, doc_chunks, doc_entity_map CREATE TABLE IF NOT EXISTS to existing migration block |
| `crates/gdpr-api/src/handlers/documents.rs` | Implement stubs | post_document, list_documents, delete_document |
| `crates/gdpr-api/src/handlers/search.rs` | Implement stub | post_search |
| `crates/gdpr-api/src/handlers/audit.rs` | Implement stub | get_review_queue |
| `crates/gdpr-mcp/src/api_client.rs` | **Create** | typed HTTP client, 30s timeout |
| `crates/gdpr-mcp/src/mcp/tools.rs` | Replace bodies | 8 tools → ApiClient calls |
| `crates/gdpr-mcp/src/proxy/mod.rs` | Modify | swap engine_pool → api_client, add legal_basis |
| `crates/gdpr-mcp/src/main.rs` | Modify | remove CoreState + vault key check, add ApiClient |
| `crates/gdpr-mcp/Cargo.toml` | Modify | remove heavy deps (see list above) |

## Non-Goals

- No kreuzberg (file/PDF extraction) in `POST /v1/documents` for T5 — text-only ingest. File upload deferred to T6+.
- No retry logic in gdpr-mcp
- No fallback to in-process mode if gdpr-api is down
- No changes to MCP tool names or parameter schemas (wire-compatible)
- No changes to the standalone `opencode/gdpr-mcp/` monolith
