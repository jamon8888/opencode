# T5 — gdpr-mcp Thin Client Design

## Goal

Rewrite `gdpr-mcp` from a fat in-process monolith (owns PII engine, NER, SQLite, kreuzberg) into a thin HTTP client that delegates all business logic to `gdpr-api` over REST. gdpr-mcp becomes a pure protocol adapter: MCP stdio ↔ HTTP.

## Context

`gdpr-mcp` currently embeds `gdpr-core` as a compile-time dependency, running ML models (GLiNER ONNX), SQLite, AES-256-GCM vault, and kreuzberg document extraction in-process. This duplicates everything `gdpr-api` already provides. The binary is ~200MB and requires ONNX/cmake at build time.

After T5 the binary carries only `rmcp` (MCP protocol), `reqwest` (HTTP), `serde_json`, and `tokio`. Binary drops to ~5MB. All capability lives in `gdpr-api`.

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

The anonymization proxy (`POST :8080/openai/v1/chat/completions`) stays in gdpr-mcp but replaces `engine_pool.anonymize_batch()` with `POST /v1/anonymize` and `POST /v1/deanonymize` calls.

## Configuration

Two environment variables read at startup. Missing either → panic with clear message, do not start.

| Variable | Example | Purpose |
|----------|---------|---------|
| `GDPR_API_URL` | `http://gdpr-api:3001` | Base URL of gdpr-api |
| `GDPR_API_KEY` | `gdpr_key_abc123` | Bearer token for all requests |

## Two-Part Implementation

### Part 1 — Complete gdpr-api Stub Handlers

Three gdpr-api handlers are currently stubs and must be real before gdpr-mcp can delegate to them.

#### `POST /v1/documents` — Document Ingest

**Request:**
```json
{ "text": "...", "legal_basis": "consent", "profile": "default" }
```

**Implementation:**
1. Extract text (already provided as `text` field — no kreuzberg call needed for plain text; kreuzberg path deferred)
2. Run `EnginePool::anonymize_batch` via `state.engine_pool`
3. Chunk with `chunk_text` from gdpr-core
4. Store in SQLite (`documents`, `doc_chunks`, `doc_entity_map` tables)
5. Write ClickHouse audit trail
6. Return `{ doc_id, pii_count, ner_degraded, ai_act_risk_level }`

**Response:**
```json
{ "doc_id": "uuid", "pii_count": 12, "ner_degraded": false, "ai_act_risk_level": "medium" }
```

#### `POST /v1/search` — Semantic/Keyword Search

**Request:**
```json
{ "query": "...", "limit": 10, "profile": "default" }
```

**Implementation:**
1. Try VecStore semantic search if `state.vec_store` is `Some`
2. Fall back to SQLite `LIKE` query on `doc_chunks.content`
3. Return top-N matching chunks with doc_id and score

**Response:**
```json
{ "results": [{ "doc_id": "uuid", "chunk": "...", "score": 0.92 }] }
```

#### `GET /v1/review-queue` — High-PII Document Queue

**Implementation:**
Query `doc_entity_map` grouped by `doc_id`, order by entity count descending, return top 20.

**Response:**
```json
{ "documents": [{ "doc_id": "uuid", "entity_count": 47, "created_at": "..." }] }
```

---

### Part 2 — gdpr-mcp Thin Client Rewrite

#### New: `crates/gdpr-mcp/src/api_client.rs`

Single struct wrapping `reqwest::Client` with typed methods for each gdpr-api endpoint.

```rust
pub struct ApiClient {
    client:   reqwest::Client,
    base_url: String,
    api_key:  String,
}

impl ApiClient {
    pub fn from_env() -> Self { /* reads GDPR_API_URL + GDPR_API_KEY, panics if missing */ }

    pub async fn anonymize(&self, req: AnonymizeRequest) -> Result<AnonymizeResponse, ApiError>;
    pub async fn deanonymize(&self, req: DeanonymizeRequest) -> Result<DeanonymizeResponse, ApiError>;
    pub async fn ingest_document(&self, req: IngestRequest) -> Result<IngestResponse, ApiError>;
    pub async fn search(&self, req: SearchRequest) -> Result<SearchResponse, ApiError>;
    pub async fn get_audit(&self, doc_id: Option<&str>, limit: u32) -> Result<AuditResponse, ApiError>;
    pub async fn delete_document(&self, doc_id: &str) -> Result<(), ApiError>;
    pub async fn list_documents(&self) -> Result<ListDocumentsResponse, ApiError>;
    pub async fn get_review_queue(&self) -> Result<ReviewQueueResponse, ApiError>;
}
```

All methods add `Authorization: Bearer <api_key>` header. All serialize request body as JSON, deserialize response as JSON.

#### `ApiError`

```rust
pub enum ApiError {
    Http { status: u16, detail: String },   // 4xx/5xx from gdpr-api
    Network(String),                         // reqwest error
}

impl std::fmt::Display for ApiError {
    // "gdpr-api error 429: usage cap exceeded: ..."
    // "gdpr-api unavailable: connection refused"
}
```

MCP tool handlers convert `ApiError` to an MCP error string. No retries — callers handle retry logic.

#### Modified: `crates/gdpr-mcp/src/mcp/tools.rs`

Each of the 8 tool handlers becomes a thin wrapper:

| MCP Tool | ApiClient call |
|----------|---------------|
| `gdpr_ingest` | `api_client.ingest_document(...)` |
| `gdpr_search` | `api_client.search(...)` |
| `gdpr_audit` | `api_client.get_audit(...)` |
| `gdpr_delete` | `api_client.delete_document(...)` |
| `gdpr_anonymize` | `api_client.anonymize(...)` |
| `gdpr_deanonymize` | `api_client.deanonymize(...)` |
| `gdpr_list_documents` | `api_client.list_documents()` |
| `gdpr_review_queue` | `api_client.get_review_queue()` |

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
api_client.anonymize(AnonymizeRequest { text: joined_slots, ... }).await
// ...
api_client.deanonymize(DeanonymizeRequest { text: rehydrate_input, ... }).await
```

The proxy's HTTP server on `:8080`, the slot collection/rehydration logic (`collect_text_slots`, `write_text_slots`), and TensorZero forwarding are **unchanged**. Only the anonymization internals are replaced.

#### Modified: `crates/gdpr-mcp/src/main.rs`

Remove `CoreState` construction (engine pool loading, SQLite open, VecStore init). Replace with:

```rust
let api_client = Arc::new(ApiClient::from_env());
```

Pass `api_client` into both `McpState` and `ProxyState`.

#### Modified: `crates/gdpr-mcp/Cargo.toml`

**Remove:**
- `gdpr-core`
- `cloakpipe-core`
- `gline-rs`
- `kreuzberg`
- `rusqlite` / `deadpool-sqlite`

**Keep:**
- `rmcp` (MCP protocol)
- `reqwest` (HTTP client — already present)
- `tokio`, `serde`, `serde_json`, `tracing`, `axum` (proxy HTTP server)

## Error Handling

| Scenario | Behavior |
|----------|----------|
| `GDPR_API_URL` or `GDPR_API_KEY` missing at startup | panic with message: `"T5: GDPR_API_URL must be set"` |
| gdpr-api returns 4xx | MCP tool error with `detail` field from API response |
| gdpr-api returns 5xx or network failure | MCP tool error: `"gdpr-api unavailable: <reason>"` |
| Proxy anonymize call fails | HTTP 502 to chat client |
| Proxy deanonymize call fails | HTTP 502 to chat client |

No retries in gdpr-mcp — callers (AI agents, chat clients) own retry logic.

## Testing

### gdpr-api stub handlers (Part 1)
- TDD: write failing tests first using real SQLite (existing pattern)
- Each handler tested via `axum::test` helpers
- `post_document`: verify doc stored, pii_count returned, audit written
- `post_search`: verify chunk results returned for matching query
- `get_review_queue`: verify ordering by entity count

### ApiClient (Part 2)
- Unit tests using `wiremock` to mock gdpr-api responses
- Test each method: happy path, 4xx error, network failure
- Verify `Authorization: Bearer` header is always sent

### MCP tools
- Integration tests spinning up a real `gdpr-api` instance (in-process) alongside gdpr-mcp tool handlers
- Existing MCP tool test structure preserved — only the handler bodies change

## File Map

| File | Action | Notes |
|------|--------|-------|
| `crates/gdpr-api/src/handlers/documents.rs` | Implement stub | post_document |
| `crates/gdpr-api/src/handlers/search.rs` | Implement stub | post_search |
| `crates/gdpr-api/src/handlers/review_queue.rs` | Implement stub | get_review_queue |
| `crates/gdpr-mcp/src/api_client.rs` | **Create** | typed HTTP client |
| `crates/gdpr-mcp/src/mcp/tools.rs` | Replace bodies | 8 tools → ApiClient calls |
| `crates/gdpr-mcp/src/proxy/mod.rs` | Modify | swap engine_pool → api_client |
| `crates/gdpr-mcp/src/main.rs` | Modify | remove CoreState, add ApiClient |
| `crates/gdpr-mcp/Cargo.toml` | Modify | remove heavy deps |

## Non-Goals

- No kreuzberg (file/PDF extraction) in `POST /v1/documents` for T5 — text-only ingest. File upload deferred.
- No retry logic in gdpr-mcp
- No fallback to in-process mode if gdpr-api is down
- No changes to MCP tool names or parameter schemas (wire-compatible)
