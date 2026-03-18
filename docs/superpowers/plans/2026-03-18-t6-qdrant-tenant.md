# T6 — Qdrant Semantic Search + Tenant Isolation Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire Qdrant semantic search into `gdpr-api`, enforce per-tenant row isolation across all document tables and Qdrant points, and return `session_id` from `post_document` so callers can deanonymize document-scoped responses.

**Architecture:** Two new tenant-scoped methods (`upsert_chunks_tenant`, `search_tenant`) are added to `QdrantStore` in `gdpr-core`. The `gdpr-api` AppState replaces the `VecStore` stub with an `Option<Arc<QdrantStore>>`, migrates SQLite tables to add `tenant_id` columns, and implements the full `post_document` / `list_documents` / `delete_document` / `post_search` / `get_audit` handlers with tenant isolation enforced at the SQL and Qdrant query level.

**Tech Stack:** Rust, axum 0.8, rusqlite (deadpool-sqlite), Qdrant REST API (reqwest), uuid v5 (new), wiremock (tests), tokio::spawn fire-and-forget

---

## File Map

| File | Action |
|------|--------|
| `hacienda/Cargo.toml` | Add `"v5"` to uuid features |
| `hacienda/crates/gdpr-core/src/clients/qdrant.rs` | Add `upsert_chunks_tenant` + `search_tenant` methods |
| `hacienda/crates/gdpr-core/Cargo.toml` | Add `wiremock = "0.6"` dev-dependency |
| `hacienda/crates/gdpr-api/src/state.rs` | Remove `VecStore` stub; add `qdrant: Option<Arc<QdrantStore>>` |
| `hacienda/crates/gdpr-api/src/main.rs` | Schema migrations (CREATE + ALTER TABLE); init `QdrantStore::from_env()` |
| `hacienda/crates/gdpr-api/src/handlers/documents.rs` | Full `post_document` + `list_documents` + `delete_document` with tenant_id + session_id + Qdrant |
| `hacienda/crates/gdpr-api/src/handlers/search.rs` | Qdrant-first search with LIKE fallback, `tenant_id` filter on both paths |
| `hacienda/crates/gdpr-api/src/handlers/audit.rs` | Add `tenant_id` WHERE clause; restore `doc_id` optional filter |
| `hacienda/crates/gdpr-api/Cargo.toml` | Add `wiremock = "0.6"` dev-dependency |

---

## Chunk 1: gdpr-core — New QdrantStore Methods

### Task 1: Enable uuid v5 feature

**Files:**
- Modify: `hacienda/Cargo.toml:12`

- [ ] **Step 1: Update the workspace uuid dependency**

In `hacienda/Cargo.toml` line 12, change:
```toml
uuid = { version = "1", features = ["v4"] }
```
to:
```toml
uuid = { version = "1", features = ["v4", "v5"] }
```

- [ ] **Step 2: Verify it compiles**

Run from `hacienda/`:
```bash
cargo check -p gdpr-core 2>&1 | tail -5
```
Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add hacienda/Cargo.toml
git commit -m "chore(workspace): enable uuid v5 feature for Qdrant tenant point IDs"
```

---

### Task 2: Add `upsert_chunks_tenant` to QdrantStore (TDD)

**Files:**
- Modify: `hacienda/crates/gdpr-core/src/clients/qdrant.rs`
- Modify: `hacienda/crates/gdpr-core/Cargo.toml` (add wiremock dev-dep)

**Context:** `QdrantStore` lives in `gdpr-core/src/clients/qdrant.rs`. It uses `reqwest::Client` for HTTP calls, `EmbeddingClient` for embeddings. The new method must:
- Accept `doc_id: &str`, `tenant_id: &str`, `chunks: &[(usize, String)]`
- Compute a deterministic UUID v5 point ID per chunk using `Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("{tenant_id}/{doc_id}/{chunk_idx}").as_bytes())`
- Embed each chunk text via `self.embedding`
- PUT all points to `/collections/{collection}/points` with payload including `doc_id`, `tenant_id`, `chunk_idx`, `chunk_text`

- [ ] **Step 1: Add wiremock dev-dependency**

In `hacienda/crates/gdpr-core/Cargo.toml`, add:
```toml
[dev-dependencies]
wiremock = "0.6"
tokio    = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Add at the bottom of `gdpr-core/src/clients/qdrant.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path};

    fn make_store(url: &str) -> QdrantStore {
        QdrantStore {
            client:     Client::new(),
            url:        url.to_string(),
            collection: "test_col".to_string(),
            embedding:  None,
            dim:        384,
        }
    }

    #[tokio::test]
    async fn test_upsert_chunks_tenant_point_ids_are_deterministic() {
        // Build two separate stores pointing at the same mock server
        let server = MockServer::start().await;

        Mock::given(method("PUT"))
            .and(path("/collections/test_col/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok", "result": {}})))
            .mount(&server)
            .await;

        // Store with no embedding client falls through embed step
        // We only test that the point IDs are deterministic (same input → same UUID v5)
        let store = make_store(&server.uri());
        // Two separate calls with same inputs must produce the same point ID
        let chunks: Vec<(usize, String)> = vec![(0, "Hello world".to_string())];
        let id1 = Uuid::new_v5(&Uuid::NAMESPACE_URL, "tenant1/doc1/0".as_bytes());
        let id2 = Uuid::new_v5(&Uuid::NAMESPACE_URL, "tenant1/doc1/0".as_bytes());
        assert_eq!(id1, id2);

        // With no embedding client, upsert skips HTTP call and returns Ok
        let result = store.upsert_chunks_tenant("doc1", "tenant1", &chunks).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_upsert_chunks_tenant_sends_correct_payload() {
        let server = MockServer::start().await;

        // Mock the embedding endpoint
        let emb_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": vec![0.1f32; 384] }]
            })))
            .mount(&emb_server)
            .await;

        // Capture the upsert request
        Mock::given(method("PUT"))
            .and(path("/collections/test_col/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok", "result": {}})))
            .mount(&server)
            .await;

        let store = QdrantStore {
            client:     Client::new(),
            url:        server.uri(),
            collection: "test_col".to_string(),
            embedding:  Some(Arc::new(EmbeddingClient::new(
                format!("{}/", emb_server.uri()),
                "test-model".to_string(),
            ))),
            dim: 384,
        };

        let chunks = vec![(0usize, "chunk zero text".to_string())];
        store.upsert_chunks_tenant("doc42", "acme", &chunks).await.unwrap();

        // Verify request was received
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);

        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let point = &body["points"][0];
        assert_eq!(point["payload"]["doc_id"], "doc42");
        assert_eq!(point["payload"]["tenant_id"], "acme");
        assert_eq!(point["payload"]["chunk_idx"], 0);
        assert_eq!(point["payload"]["chunk_text"], "chunk zero text");

        // Verify point ID is the expected UUID v5
        let expected_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, "acme/doc42/0".as_bytes()).to_string();
        assert_eq!(point["id"], expected_id);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cd hacienda && cargo test -p gdpr-core test_upsert_chunks_tenant 2>&1 | tail -10
```
Expected: `error[E0425]: cannot find function 'upsert_chunks_tenant'` or similar compile error.

- [ ] **Step 4: Implement `upsert_chunks_tenant`**

First, mark the private `client` and `url` fields as `pub(crate)` so the test module can build `QdrantStore` directly. In `qdrant.rs` change lines 70–71:
```rust
// Before:
    client: Client,
    url: String,
// After:
    pub(crate) client: Client,
    pub(crate) url: String,
```

Then add this method to `impl QdrantStore` in `gdpr-core/src/clients/qdrant.rs`, after the existing `upsert_chunks` method (around line 245):

```rust
/// Embed and upsert multiple text chunks, storing `tenant_id` in each point payload.
/// Point IDs are deterministic UUID v5 from `"{tenant_id}/{doc_id}/{chunk_idx}"`.
/// This supersedes `upsert_chunks` for T6+ ingest paths.
pub async fn upsert_chunks_tenant(
    &self,
    doc_id:    &str,
    tenant_id: &str,
    chunks:    &[(usize, String)],
) -> anyhow::Result<()> {
    let Some(ref emb) = self.embedding else {
        tracing::warn!(doc_id, "EMBEDDING_URL not set — skipping Qdrant tenant upsert");
        return Ok(());
    };

    let mut points = Vec::with_capacity(chunks.len());
    for (chunk_idx, chunk_text) in chunks {
        let vector   = emb.embed(chunk_text).await?;
        let point_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("{tenant_id}/{doc_id}/{chunk_idx}").as_bytes(),
        );
        points.push(json!({
            "id":      point_id.to_string(),
            "vector":  vector,
            "payload": {
                "doc_id":     doc_id,
                "tenant_id":  tenant_id,
                "chunk_idx":  chunk_idx,
                "chunk_text": chunk_text,
            }
        }));
    }

    if points.is_empty() {
        return Ok(());
    }

    self.client
        .put(format!("{}/collections/{}/points", self.url, self.collection))
        .json(&json!({ "points": points }))
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd hacienda && cargo test -p gdpr-core test_upsert_chunks_tenant 2>&1 | tail -10
```
Expected: `2 passed`.

- [ ] **Step 6: Commit**

```bash
git add hacienda/Cargo.toml hacienda/crates/gdpr-core/Cargo.toml hacienda/crates/gdpr-core/src/clients/qdrant.rs
git commit -m "feat(gdpr-core): add upsert_chunks_tenant with UUID v5 point IDs and tenant payload"
```

---

### Task 3: Add `search_tenant` to QdrantStore (TDD)

**Files:**
- Modify: `hacienda/crates/gdpr-core/src/clients/qdrant.rs`

**Context:** `search_tenant` embeds a query and sends a POST to `/collections/{collection}/points/search` with a `must: [{ key: "tenant_id", match: { value: tenant_id } }]` filter. Returns `Vec<QdrantHit>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `qdrant.rs`:
```rust
    #[tokio::test]
    async fn test_search_tenant_sends_filter() {
        let server     = MockServer::start().await;
        let emb_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{ "embedding": vec![0.1f32; 384] }]
            })))
            .mount(&emb_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/collections/test_col/points/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "score": 0.95,
                    "payload": {
                        "doc_id":     "doc99",
                        "tenant_id":  "acme",
                        "chunk_idx":  0,
                        "chunk_text": "some text"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let store = QdrantStore {
            client:     Client::new(),
            url:        server.uri(),
            collection: "test_col".to_string(),
            embedding:  Some(Arc::new(EmbeddingClient::new(
                format!("{}/", emb_server.uri()),
                "test-model".to_string(),
            ))),
            dim: 384,
        };

        let hits = store.search_tenant("find something", "acme", 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "doc99");
        assert!((hits[0].score - 0.95).abs() < 0.001);
        assert_eq!(hits[0].chunk_text.as_deref(), Some("some text"));

        // Verify the filter was sent in the request body
        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        let must = &body["filter"]["must"][0];
        assert_eq!(must["key"], "tenant_id");
        assert_eq!(must["match"]["value"], "acme");
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cd hacienda && cargo test -p gdpr-core test_search_tenant_sends_filter 2>&1 | tail -5
```
Expected: compile error — `search_tenant` not found.

- [ ] **Step 3: Implement `search_tenant`**

Add to `impl QdrantStore` in `qdrant.rs`, after `upsert_chunks_tenant`:

```rust
/// Embed query, search Qdrant filtered by `tenant_id`.
/// Returns at most `limit` hits ordered by score descending.
pub async fn search_tenant(
    &self,
    query:     &str,
    tenant_id: &str,
    limit:     u64,
) -> anyhow::Result<Vec<QdrantHit>> {
    let Some(ref emb) = self.embedding else {
        return Err(anyhow::anyhow!("EMBEDDING_URL not set — cannot perform vector search"));
    };

    let vector = emb.embed(query).await?;

    let resp: Value = self
        .client
        .post(format!("{}/collections/{}/points/search", self.url, self.collection))
        .json(&json!({
            "vector": vector,
            "filter": {
                "must": [{ "key": "tenant_id", "match": { "value": tenant_id } }]
            },
            "limit":        limit,
            "with_payload": true,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let hits = resp["result"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| {
            let doc_id     = r["payload"]["doc_id"].as_str()?.to_string();
            let score      = r["score"].as_f64()? as f32;
            let chunk_text = r["payload"]["chunk_text"].as_str().map(|s| s.to_string());
            let chunk_idx  = r["payload"]["chunk_idx"].as_u64().map(|v| v as usize);
            Some(QdrantHit { doc_id, score, chunk_text, chunk_idx })
        })
        .collect();

    Ok(hits)
}
```

- [ ] **Step 4: Run all gdpr-core tests**

```bash
cd hacienda && cargo test -p gdpr-core 2>&1 | tail -10
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add hacienda/crates/gdpr-core/src/clients/qdrant.rs
git commit -m "feat(gdpr-core): add search_tenant with Qdrant tenant_id filter"
```

---

## Chunk 2: gdpr-api — State, Migrations, IngestResp

### Task 4: Replace VecStore with QdrantStore in state.rs

**Files:**
- Modify: `hacienda/crates/gdpr-api/src/state.rs`

**Context:** `state.rs` currently has `pub struct VecStore;` (placeholder) and `pub vec_store: Option<Arc<VecStore>>` on `AppState`. Remove both; add `use gdpr_core::clients::qdrant::QdrantStore;` and `pub qdrant: Option<Arc<QdrantStore>>`.

- [ ] **Step 1: Edit state.rs**

In `hacienda/crates/gdpr-api/src/state.rs`:

a) Add import after `use std::sync::Arc;`:
```rust
use gdpr_core::clients::qdrant::QdrantStore;
```

b) Replace the `VecStore` stub at the bottom of the file:
```rust
// REMOVE this:
/// Placeholder for optional vector store. Real implementation comes in T6/T7.
pub struct VecStore;
```

c) In `AppState`, replace:
```rust
    // New T4 fields
    pub vec_store:           Option<Arc<VecStore>>,
```
with:
```rust
    pub qdrant:              Option<Arc<QdrantStore>>,
```

- [ ] **Step 2: Check that it compiles (expect main.rs to fail — that's fine)**

```bash
cd hacienda && cargo check -p gdpr-api 2>&1 | grep "error\[" | head -5
```
Expected: errors in `main.rs` about `vec_store` field not existing. That's expected — Task 5 fixes it.

- [ ] **Step 3: Commit state.rs change alone**

```bash
git add hacienda/crates/gdpr-api/src/state.rs
git commit -m "refactor(gdpr-api): replace VecStore stub with QdrantStore in AppState"
```

---

### Task 5: Schema migrations + QdrantStore init in main.rs

**Files:**
- Modify: `hacienda/crates/gdpr-api/src/main.rs`

**Context:** `main.rs` creates `AppState` at line 132 with `vec_store: None` (line 141) which must become `qdrant: ...`. Also need to add SQLite migrations for the document tables. The pattern for idempotent SQLite migrations is already established in `main.rs` (see lines 69-79): individual `conn.execute()` calls, catching specific error messages.

Two migration phases:
1. **CREATE TABLE IF NOT EXISTS** — creates tables with `tenant_id` baked in for fresh installs
2. **ALTER TABLE** — adds `tenant_id` to existing tables, catches "duplicate column name" silently

- [ ] **Step 1: Add wiremock dev-dep to gdpr-api Cargo.toml**

In `hacienda/crates/gdpr-api/Cargo.toml`, add:
```toml
[dev-dependencies]
wiremock = "0.6"
```

- [ ] **Step 2: Add document schema migration**

In `hacienda/crates/gdpr-api/src/main.rs`, inside the same `conn.interact(|c| { c.execute_batch(...) })` block that creates `api_keys` and `usage_records` (around line 41), add the document tables before the closing `"` of the `execute_batch` string:

```rust
                CREATE TABLE IF NOT EXISTS documents (
                    id             TEXT PRIMARY KEY,
                    original_text  TEXT NOT NULL,
                    anonymized_text TEXT NOT NULL,
                    created_at     TEXT NOT NULL,
                    tenant_id      TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_docs_tenant ON documents(tenant_id);

                CREATE TABLE IF NOT EXISTS doc_chunks (
                    id           INTEGER PRIMARY KEY AUTOINCREMENT,
                    doc_id       TEXT NOT NULL,
                    chunk_idx    INTEGER NOT NULL,
                    chunk_text   TEXT NOT NULL,
                    byte_offset  INTEGER NOT NULL DEFAULT 0,
                    tenant_id    TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_tenant ON doc_chunks(tenant_id);

                CREATE TABLE IF NOT EXISTS doc_entity_map (
                    id            INTEGER PRIMARY KEY AUTOINCREMENT,
                    document_id   TEXT NOT NULL,
                    entity_type   TEXT NOT NULL,
                    original_value TEXT NOT NULL,
                    pseudonym     TEXT NOT NULL,
                    tenant_id     TEXT NOT NULL DEFAULT ''
                );
                CREATE INDEX IF NOT EXISTS idx_dem_tenant ON doc_entity_map(tenant_id);
```

- [ ] **Step 3: Add ALTER TABLE migrations for existing databases**

After the existing `conn2.interact(...)` block (around line 79), add a new migration block:

```rust
    {
        let conn3 = api_pool.get().await?;
        conn3.interact(|c| {
            let alter_stmts = [
                "ALTER TABLE documents      ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
                "ALTER TABLE doc_chunks     ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
                "ALTER TABLE doc_entity_map ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
                "CREATE INDEX IF NOT EXISTS idx_docs_tenant    ON documents(tenant_id)",
                "CREATE INDEX IF NOT EXISTS idx_chunks_tenant  ON doc_chunks(tenant_id)",
                "CREATE INDEX IF NOT EXISTS idx_dem_tenant     ON doc_entity_map(tenant_id)",
            ];
            for stmt in &alter_stmts {
                if let Err(e) = c.execute(stmt, []) {
                    if !e.to_string().contains("duplicate column name") {
                        return Err(e);
                    }
                }
            }
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("Migration error: {e}"))?
        .map_err(|e| anyhow::anyhow!("Schema migration failed: {e}"))?;
    }
```

- [ ] **Step 4: Init QdrantStore and fix AppState literal**

After the `clickhouse` init block (around line 115), add:
```rust
    // Optional Qdrant vector store — skipped gracefully if QDRANT_URL not set
    let qdrant = gdpr_core::clients::qdrant::QdrantStore::from_env();
```

In the `AppState { ... }` literal (around line 132), replace:
```rust
        // New T4 fields
        vec_store: None,
```
with:
```rust
        qdrant,
```

- [ ] **Step 5: Verify it compiles**

```bash
cd hacienda && cargo check -p gdpr-api 2>&1 | grep "error\[" | head -10
```
Expected: no errors (or only errors in handler files that still reference `vec_store` — none should after this task).

- [ ] **Step 6: Commit**

```bash
git add hacienda/crates/gdpr-api/Cargo.toml hacienda/crates/gdpr-api/src/main.rs
git commit -m "feat(gdpr-api): document schema migrations + QdrantStore init in AppState"
```

---

## Chunk 3: gdpr-api — Handlers

### Task 6: Full `post_document`, `list_documents`, `delete_document` (TDD)

**Files:**
- Modify: `hacienda/crates/gdpr-api/src/handlers/documents.rs`

**Context:** Replace all three stubs. Current file has stubs for all three handlers plus `IngestResp` missing `session_id`, `chunk_count`, `decision_explanation`. The test helper builds an in-memory AppState. `post_document` follows the pattern in `handlers/anonymize.rs:68-92` for the anonymize step.

Key imports needed:
```rust
use std::sync::Arc;
use axum::{extract::{State, Path, Extension}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use crate::state::{AppState, AuthContext, SessionCache};
use crate::error::{ApiError, ApiResult};
use gdpr_core::pii::{AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool};
```

- [ ] **Step 1: Write the failing test module**

Add at the bottom of `handlers/documents.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Extension, State};
    use std::sync::Arc;
    use dashmap::DashMap;
    use deadpool_sqlite::Config;

    /// Build a minimal in-memory AppState for document handler tests.
    /// engine_pool is needed for the struct but post_document doesn't use it.
    async fn make_state() -> AppState {
        let cfg  = Config::new(":memory:");
        let pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        let conn = pool.get().await.unwrap();
        conn.interact(|c| {
            c.execute_batch("
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY, original_text TEXT NOT NULL,
                    anonymized_text TEXT NOT NULL, created_at TEXT NOT NULL,
                    tenant_id TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE IF NOT EXISTS doc_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, doc_id TEXT NOT NULL,
                    chunk_idx INTEGER NOT NULL, chunk_text TEXT NOT NULL,
                    byte_offset INTEGER NOT NULL DEFAULT 0,
                    tenant_id TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE IF NOT EXISTS doc_entity_map (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, document_id TEXT NOT NULL,
                    entity_type TEXT NOT NULL, original_value TEXT NOT NULL,
                    pseudonym TEXT NOT NULL, tenant_id TEXT NOT NULL DEFAULT ''
                );
            ")
        }).await.unwrap().unwrap();

        // engine_pool: post_document doesn't use it, but AppState requires the field
        let vault = std::env::temp_dir()
            .join(format!("gdpr-t6-test-{}.db", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("CLOAKPIPE_VAULT_KEY", "test-vault-key-32bytespadded!!") };
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(
            vault.to_str().unwrap(),
        ).expect("test engine");
        let engine_pool = Arc::new(
            gdpr_core::pii::pool::EnginePool::new(1, engine).expect("test pool"),
        );

        let billing_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://localhost:8123"));
        AppState {
            db:                  pool,
            keys:                Arc::new(DashMap::new()),
            http:                reqwest::Client::new(),
            upstream_url:        "http://localhost:3000".to_string(),
            session_cache:       Arc::new(DashMap::new()),
            engine_pool,
            clickhouse:          None,
            qdrant:              None,
            http_client:         reqwest::Client::new(),
            key_cache:           Arc::new(DashMap::new()),
            tensorzero_base_url: "".to_string(),
            tensorzero_key:      "".to_string(),
            jwt_secret:          "".to_string(),
            meter:               Arc::new(gdpr_billing::Meter::new(billing_ch)),
            snapshot_cache:      Arc::new(DashMap::new()),
        }
    }

    fn auth(tenant: &str) -> AuthContext {
        AuthContext {
            tenant_id:  tenant.to_string(),
            api_key_id: "key1".to_string(),
            scopes:     vec!["*".to_string()],
            plan:       gdpr_billing::Plan::Starter,
        }
    }

    #[tokio::test]
    async fn test_post_document_stores_tenant_id() {
        let state = make_state().await;
        let req = IngestReq {
            text:        "Hello world, contact jean.dupont@example.com".to_string(),
            legal_basis: Some("consent".to_string()),
            profile:     Some("max".to_string()),
        };
        let resp = post_document(
            State(state.clone()),
            Extension(auth("tenant_a")),
            Json(req),
        ).await.expect("post_document failed");

        let (status, Json(body)) = resp;
        assert_eq!(status, StatusCode::CREATED);
        assert!(!body.session_id.is_empty());
        // UUID parse confirms session_id is a valid UUID
        uuid::Uuid::parse_str(&body.session_id).expect("session_id must be valid UUID");

        // Verify tenant_id stored in documents
        let conn = state.db.get().await.unwrap();
        let (count, stored_tenant): (i64, String) = conn.interact(move |c| {
            c.query_row(
                "SELECT COUNT(*), tenant_id FROM documents WHERE id = ?1",
                [&body.doc_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        }).await.unwrap().unwrap();
        assert_eq!(count, 1);
        assert_eq!(stored_tenant, "tenant_a");
    }

    #[tokio::test]
    async fn test_ingest_resp_has_session_id() {
        let state = make_state().await;
        let req = IngestReq {
            text:        "Test text without PII".to_string(),
            legal_basis: None,
            profile:     None,
        };
        let (_, Json(body)) = post_document(
            State(state.clone()),
            Extension(auth("t1")),
            Json(req),
        ).await.expect("ok");
        assert!(!body.session_id.is_empty());
        assert_eq!(body.session_id.len(), 36); // UUID string length
    }

    #[tokio::test]
    async fn test_tenant_isolation_list() {
        let state = make_state().await;
        // Insert doc for tenant_a directly
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-a1", "orig", "anon", "2026-01-01T00:00:00Z", "tenant_a"],
            )?;
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-b1", "orig", "anon", "2026-01-01T00:00:00Z", "tenant_b"],
            )?;
            Ok::<_, rusqlite::Error>(())
        }).await.unwrap().unwrap();

        // tenant_a sees only its doc
        let Json(list_a) = list_documents(
            State(state.clone()),
            Extension(auth("tenant_a")),
        ).await.expect("ok");
        let ids_a: Vec<String> = list_a["items"].as_array().unwrap()
            .iter()
            .map(|v| v["doc_id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids_a.contains(&"doc-a1".to_string()));
        assert!(!ids_a.contains(&"doc-b1".to_string()));
    }

    #[tokio::test]
    async fn test_tenant_isolation_delete() {
        let state = make_state().await;
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["doc-c1", "orig", "anon", "2026-01-01T00:00:00Z", "tenant_c"],
            )
        }).await.unwrap().unwrap();

        // tenant_b tries to delete tenant_c's doc → 404
        let result = delete_document(
            State(state.clone()),
            Extension(auth("tenant_b")),
            Path("doc-c1".to_string()),
        ).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        let status = axum::response::IntoResponse::into_response(err).status();
        assert_eq!(status.as_u16(), 404);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd hacienda && cargo test -p gdpr-api test_post_document 2>&1 | tail -10
```
Expected: compile errors — `session_id` field missing, `Extension` extractor not in handler signature, etc.

- [ ] **Step 3: Implement the full `documents.rs`**

Replace the entire contents of `hacienda/crates/gdpr-api/src/handlers/documents.rs` with:

```rust
use std::sync::Arc;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use gdpr_core::pii::{
    AnonProfile, SessionContext, TreatmentEngine, anonymize_with_profile, get_pool,
};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthContext, SessionCache};

// ── Request / Response ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct IngestReq {
    pub text:        String,
    pub legal_basis: Option<String>,
    pub profile:     Option<String>,
}

#[derive(Serialize)]
pub struct IngestResp {
    pub doc_id:               String,
    pub session_id:           String,
    pub pii_count:            usize,
    pub ner_degraded:         bool,
    pub ai_act_risk_level:    String,
    pub chunk_count:          usize,
    pub decision_explanation: String,
}

// ── POST /v1/documents ────────────────────────────────────────────────────────

pub async fn post_document(
    State(state):   State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(req):       Json<IngestReq>,
) -> ApiResult<(StatusCode, Json<IngestResp>)> {
    let doc_id     = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let text       = req.text;
    let legal_basis = req.legal_basis.unwrap_or_else(|| "legitimate_interest".to_string());
    let tenant_id  = auth.tenant_id.clone();

    // Parse profile
    let profile: AnonProfile = req.profile.as_deref()
        .and_then(|p| serde_json::from_value(serde_json::Value::String(p.to_string())).ok())
        .unwrap_or_default(); // defaults to AnonProfile::Max (the #[default] variant)

    // Anonymize (L1 regex, CPU-bound)
    let mut session_ctx  = SessionContext::new(profile);
    let pool_strings: Vec<String> = get_pool(&profile).iter().map(|s| s.to_string()).collect();
    let engine  = TreatmentEngine::new(pool_strings);
    let text2   = text.clone();
    let result  = tokio::task::spawn_blocking(move || {
        anonymize_with_profile(&text2, profile, &mut session_ctx, &engine)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("spawn_blocking join: {e}")))?
    .map_err(|e| ApiError::Internal(format!("anonymize_with_profile: {e}")))?;

    // Populate session_cache (for deanonymize calls)
    {
        let dm = dashmap::DashMap::new();
        for (token, original) in &result.token_map {
            dm.insert(token.clone(), original.clone());
        }
        state.session_cache.insert(session_id.clone(), SessionCache {
            token_map:  dm,
            created_at: std::time::Instant::now(),
        });
    }

    // Chunk the anonymized text (simple whitespace-based chunking, ~200 words per chunk)
    let words: Vec<&str> = result.text.split_whitespace().collect();
    let chunk_size = 200usize;
    let chunk_rows: Vec<(usize, String, usize)> = words
        .chunks(chunk_size)
        .enumerate()
        .map(|(i, w)| (i, w.join(" "), i * chunk_size))
        .collect();

    let chunk_count = chunk_rows.len();

    // SQLite writes
    let doc_id2         = doc_id.clone();
    let anonymized_text = result.text.clone();
    let original_text   = text.clone();
    let tenant_id2      = tenant_id.clone();
    let legal_basis2    = legal_basis.clone();
    let pii_count       = result.pii_count;
    let ner_degraded    = result.ner_degraded;
    let ai_act_risk     = "low".to_string(); // simplified for now
    let chunk_rows2     = chunk_rows.clone();
    let token_map2      = result.token_map.clone();

    let conn = state.db.get().await?;
    conn.interact(move |c| {
        let now = chrono::Utc::now().to_rfc3339();
        c.execute(
            "INSERT INTO documents (id, original_text, anonymized_text, created_at, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![doc_id2, original_text, anonymized_text, now, tenant_id2],
        )?;
        for (idx, chunk_text, byte_offset) in &chunk_rows2 {
            c.execute(
                "INSERT INTO doc_chunks (doc_id, chunk_idx, chunk_text, byte_offset, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![doc_id2, idx, chunk_text, byte_offset, tenant_id2],
            )?;
        }
        for (token, original) in &token_map2 {
            c.execute(
                "INSERT INTO doc_entity_map (document_id, entity_type, original_value, pseudonym, tenant_id) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![doc_id2, "PII", original, token, tenant_id2],
            )?;
        }
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    // Fire-and-forget Qdrant upsert
    if let Some(ref q) = state.qdrant {
        let q          = Arc::clone(q);
        let chunks: Vec<(usize, String)> = chunk_rows
            .iter()
            .map(|(idx, text, _offset)| (*idx, text.clone()))
            .collect();
        let doc_id3    = doc_id.clone();
        let tenant_id3 = auth.tenant_id.clone();
        tokio::spawn(async move {
            if let Err(e) = q.upsert_chunks_tenant(&doc_id3, &tenant_id3, &chunks).await {
                tracing::warn!(error = %e, doc_id = %doc_id3, "qdrant upsert failed");
            }
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(IngestResp {
            doc_id,
            session_id,
            pii_count,
            ner_degraded,
            ai_act_risk_level:    ai_act_risk,
            chunk_count,
            decision_explanation: format!(
                "Processed with profile {:?}, {} PII entities replaced",
                profile, pii_count
            ),
        }),
    ))
}

// ── GET /v1/documents ─────────────────────────────────────────────────────────

pub async fn list_documents(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = auth.tenant_id.clone();
    let conn = state.db.get().await?;
    let items: Vec<serde_json::Value> = conn.interact(move |c| {
        let mut stmt = c.prepare(
            "SELECT d.id AS doc_id, d.created_at, COUNT(e.id) AS entity_count
             FROM documents d
             LEFT JOIN doc_entity_map e ON e.document_id = d.id
             WHERE d.tenant_id = ?1
             GROUP BY d.id
             ORDER BY d.created_at DESC",
        )?;
        let rows = stmt.query_map([&tenant_id], |r| {
            Ok(serde_json::json!({
                "doc_id":       r.get::<_, String>(0)?,
                "created_at":   r.get::<_, String>(1)?,
                "entity_count": r.get::<_, i64>(2)?,
            }))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    let total = items.len();
    Ok(Json(serde_json::json!({ "items": items, "total": total })))
}

// ── GET /v1/documents/:id ────────────────────────────────────────────────────

pub async fn get_document(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id):        Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = auth.tenant_id.clone();
    let conn = state.db.get().await?;
    let row: Option<serde_json::Value> = conn.interact(move |c| {
        c.query_row(
            "SELECT id, anonymized_text, created_at FROM documents WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![id, tenant_id],
            |r| Ok(serde_json::json!({
                "id":             r.get::<_, String>(0)?,
                "anonymized_text": r.get::<_, String>(1)?,
                "created_at":     r.get::<_, String>(2)?,
            })),
        ).optional()
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    row.map(Json).ok_or_else(|| ApiError::NotFound("document not found".to_string()))
}

// ── DELETE /v1/documents/:id ──────────────────────────────────────────────────

pub async fn delete_document(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id):        Path<String>,
) -> ApiResult<StatusCode> {
    let tenant_id = auth.tenant_id.clone();
    let id2       = id.clone();
    let conn = state.db.get().await?;

    let deleted: bool = conn.interact(move |c| {
        // 404 guard: verify ownership
        let exists: bool = c.query_row(
            "SELECT id FROM documents WHERE id = ?1 AND tenant_id = ?2",
            rusqlite::params![id2, tenant_id],
            |_| Ok(true),
        ).optional()?.unwrap_or(false);

        if !exists {
            return Ok(false);
        }

        c.execute("DELETE FROM doc_chunks     WHERE doc_id      = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        c.execute("DELETE FROM doc_entity_map WHERE document_id = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        c.execute("DELETE FROM documents      WHERE id          = ?1 AND tenant_id = ?2", rusqlite::params![id2, tenant_id])?;
        Ok::<_, rusqlite::Error>(true)
    })
    .await
    .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
    .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    if !deleted {
        return Err(ApiError::NotFound(format!("document '{}' not found", id)));
    }

    // Fire-and-forget Qdrant delete
    if let Some(ref q) = state.qdrant {
        let q    = Arc::clone(q);
        let id3  = id.clone();
        tokio::spawn(async move {
            if let Err(e) = q.delete(&id3).await {
                tracing::warn!(error = %e, doc_id = %id3, "qdrant delete failed");
            }
        });
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    // ... (test module written in Step 1 above — do not duplicate)
}
```

- [ ] **Step 4: Run document handler tests**

```bash
cd hacienda && cargo test -p gdpr-api test_post_document 2>&1 | tail -10
cd hacienda && cargo test -p gdpr-api test_ingest_resp 2>&1 | tail -10
cd hacienda && cargo test -p gdpr-api test_tenant_isolation 2>&1 | tail -10
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add hacienda/crates/gdpr-api/src/handlers/documents.rs
git commit -m "feat(gdpr-api): full post_document + list/delete with tenant_id isolation and session_id"
```

---

### Task 7: Implement `post_search` with Qdrant-first + LIKE fallback (TDD)

**Files:**
- Modify: `hacienda/crates/gdpr-api/src/handlers/search.rs`

**Context:** If `state.qdrant` is `Some`, call `search_tenant`. On success return results. On failure return `ApiError::Internal` — do NOT silently fall back. If `state.qdrant` is `None`, use SQLite LIKE query scoped to `tenant_id`.

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `search.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Extension, State};
    use std::sync::Arc;
    use dashmap::DashMap;
    use deadpool_sqlite::Config;
    use wiremock::{MockServer, Mock, ResponseTemplate};
    use wiremock::matchers::{method, path};

    async fn make_state_no_qdrant() -> AppState {
        let cfg  = Config::new(":memory:");
        let pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1).unwrap();
        let conn = pool.get().await.unwrap();
        conn.interact(|c| {
            c.execute_batch("
                CREATE TABLE doc_chunks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    doc_id TEXT NOT NULL, chunk_idx INTEGER NOT NULL,
                    chunk_text TEXT NOT NULL, byte_offset INTEGER NOT NULL DEFAULT 0,
                    tenant_id TEXT NOT NULL DEFAULT ''
                );
            ")
        }).await.unwrap().unwrap();

        let vault = std::env::temp_dir()
            .join(format!("gdpr-search-test-{}.db", uuid::Uuid::new_v4()));
        let engine = gdpr_core::pii::engine::PiiEngine::load_for_test(vault.to_str().unwrap()).unwrap();
        let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(1, engine).unwrap());
        let billing_ch = Arc::new(gdpr_core::clients::ClickHouseClient::new("http://localhost:8123"));

        AppState {
            db:                  pool,
            keys:                Arc::new(DashMap::new()),
            http:                reqwest::Client::new(),
            upstream_url:        "".to_string(),
            session_cache:       Arc::new(DashMap::new()),
            engine_pool,
            clickhouse:          None,
            qdrant:              None,
            http_client:         reqwest::Client::new(),
            key_cache:           Arc::new(DashMap::new()),
            tensorzero_base_url: "".to_string(),
            tensorzero_key:      "".to_string(),
            jwt_secret:          "".to_string(),
            meter:               Arc::new(gdpr_billing::Meter::new(billing_ch)),
            snapshot_cache:      Arc::new(DashMap::new()),
        }
    }

    fn auth(tenant: &str) -> crate::state::AuthContext {
        crate::state::AuthContext {
            tenant_id:  tenant.to_string(),
            api_key_id: "k1".to_string(),
            scopes:     vec!["*".to_string()],
            plan:       gdpr_billing::Plan::Starter,
        }
    }

    #[tokio::test]
    async fn test_search_falls_back_to_like_when_no_qdrant() {
        let state = make_state_no_qdrant().await;
        // Insert a chunk for tenant_a
        let conn = state.db.get().await.unwrap();
        conn.interact(|c| {
            c.execute(
                "INSERT INTO doc_chunks (doc_id, chunk_idx, chunk_text, byte_offset, tenant_id) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params!["doc1", 0, "hello world test data", 0, "tenant_a"],
            )
        }).await.unwrap().unwrap();

        let Json(resp) = post_search(
            State(state.clone()),
            Extension(auth("tenant_a")),
            Json(SearchReq { query: "hello".to_string(), limit: Some(10), profile: None }),
        ).await.expect("ok");

        assert!(!resp.results.is_empty());
        assert_eq!(resp.results[0]["doc_id"], "doc1");

        // tenant_b sees nothing
        let Json(resp_b) = post_search(
            State(state),
            Extension(auth("tenant_b")),
            Json(SearchReq { query: "hello".to_string(), limit: None, profile: None }),
        ).await.expect("ok");
        assert!(resp_b.results.is_empty());
    }

    #[tokio::test]
    async fn test_search_uses_qdrant_when_available() {
        let qdrant_server = MockServer::start().await;
        let emb_server    = MockServer::start().await;

        Mock::given(method("POST")).and(path("/embeddings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"embedding": vec![0.1f32; 384]}]
            })))
            .mount(&emb_server).await;

        Mock::given(method("POST")).and(path("/collections/gdpr_docs/points/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": [{
                    "score": 0.9,
                    "payload": {"doc_id": "qdoc1", "tenant_id": "t1", "chunk_idx": 0, "chunk_text": "qdrant result"}
                }]
            })))
            .mount(&qdrant_server).await;

        let mut state = make_state_no_qdrant().await;
        std::env::set_var("QDRANT_URL", qdrant_server.uri());
        std::env::set_var("EMBEDDING_URL", format!("{}/", emb_server.uri()));
        state.qdrant = gdpr_core::clients::qdrant::QdrantStore::from_env();

        let Json(resp) = post_search(
            State(state),
            Extension(auth("t1")),
            Json(SearchReq { query: "find qdrant".to_string(), limit: Some(5), profile: None }),
        ).await.expect("ok");

        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0]["doc_id"], "qdoc1");

        // Verify tenant_id filter was sent
        let reqs = qdrant_server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        let must = &body["filter"]["must"][0];
        assert_eq!(must["key"], "tenant_id");
        assert_eq!(must["match"]["value"], "t1");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd hacienda && cargo test -p gdpr-api test_search 2>&1 | tail -10
```
Expected: compile/logic errors.

- [ ] **Step 3: Implement `search.rs`**

Replace the entire contents of `handlers/search.rs`:

```rust
use axum::{extract::{Extension, State}, Json};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, AuthContext};

#[derive(Deserialize)]
pub struct SearchReq {
    pub query:   String,
    pub limit:   Option<usize>,
    pub profile: Option<String>,
}

#[derive(Serialize)]
pub struct SearchResp {
    pub results: Vec<serde_json::Value>,
}

pub async fn post_search(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(req):       Json<SearchReq>,
) -> ApiResult<Json<SearchResp>> {
    let limit     = req.limit.unwrap_or(10).min(100) as u64;
    let tenant_id = auth.tenant_id.clone();

    // Qdrant path — if available
    if let Some(ref q) = state.qdrant {
        let hits = q
            .search_tenant(&req.query, &tenant_id, limit)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

        let results = hits
            .into_iter()
            .map(|h| serde_json::json!({
                "doc_id":     h.doc_id,
                "chunk":      h.chunk_text.unwrap_or_default(),
                "score":      h.score,
                "chunk_idx":  h.chunk_idx,
            }))
            .collect();

        return Ok(Json(SearchResp { results }));
    }

    // LIKE fallback — scoped to tenant_id
    let query_pat = format!("%{}%", req.query.replace('%', "\\%").replace('_', "\\_"));
    let conn = state.db.get().await?;
    let results: Vec<serde_json::Value> = conn
        .interact(move |c| {
            let mut stmt = c.prepare(
                "SELECT doc_id, chunk_text, chunk_idx \
                 FROM doc_chunks \
                 WHERE tenant_id = ?1 AND chunk_text LIKE ?2 ESCAPE '\\' \
                 ORDER BY chunk_idx \
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![tenant_id, query_pat, limit as i64],
                |r| Ok(serde_json::json!({
                    "doc_id":    r.get::<_, String>(0)?,
                    "chunk":     r.get::<_, String>(1)?,
                    "score":     1.0f64,
                    "chunk_idx": r.get::<_, i64>(2)?,
                })),
            )?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await
        .map_err(|e| ApiError::Internal(format!("db interact: {e}")))?
        .map_err(|e: rusqlite::Error| ApiError::Database(e.to_string()))?;

    Ok(Json(SearchResp { results }))
}

#[cfg(test)]
mod tests {
    // ... (test module written in Step 1)
}
```

- [ ] **Step 4: Run tests**

```bash
cd hacienda && cargo test -p gdpr-api test_search 2>&1 | tail -10
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add hacienda/crates/gdpr-api/src/handlers/search.rs
git commit -m "feat(gdpr-api): Qdrant-first search with LIKE fallback, tenant_id scoped"
```

---

### Task 8: Implement `get_audit` with tenant_id + doc_id filter (TDD)

**Files:**
- Modify: `hacienda/crates/gdpr-api/src/handlers/audit.rs`

**Context:** The existing `get_audit` handler (line 29) doesn't scope by `tenant_id` and doesn't have a `doc_id` filter. The ClickHouse client uses HTTP GET with URL-encoded raw SQL. Follow the existing `action` filter pattern: build filter strings via `format!`, validate `doc_id` as UUID before interpolating. Also update `AuditQuery` to add `doc_id: Option<String>`.

- [ ] **Step 1: Write the failing tests**

Add at the bottom of `audit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_query_accepts_doc_id() {
        // Verify AuditQuery can deserialize doc_id field
        let q: AuditQuery = serde_json::from_str(r#"{"doc_id":"550e8400-e29b-41d4-a716-446655440000","limit":5}"#).unwrap();
        assert_eq!(q.doc_id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
        assert_eq!(q.limit, Some(5));
    }

    #[test]
    fn test_audit_rejects_non_uuid_doc_id() {
        // Simulates the validation logic
        let id = "'; DROP TABLE audit_log; --";
        let valid = uuid::Uuid::parse_str(id).is_ok();
        assert!(!valid); // must be rejected
    }

    #[test]
    fn test_audit_query_without_doc_id() {
        let q: AuditQuery = serde_json::from_str(r#"{"limit":20}"#).unwrap();
        assert!(q.doc_id.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they compile and the doc_id test fails**

```bash
cd hacienda && cargo test -p gdpr-api test_audit 2>&1 | tail -10
```
Expected: `test_audit_query_accepts_doc_id` fails because `AuditQuery` has no `doc_id` field.

- [ ] **Step 3: Implement the updated `audit.rs`**

Replace the `AuditQuery` struct and `get_audit` function (keep existing `AuditEvent`, `get_review_queue` stubs). Replace lines 7-94 of `audit.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit:  Option<u32>,
    pub action: Option<String>,
    pub doc_id: Option<String>,
}

pub async fn get_audit(
    State(state):    State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(params):   Query<AuditQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(ch) = &state.clickhouse else {
        return Ok(Json(serde_json::json!({
            "events": [],
            "total":  0,
            "note":   "ClickHouse not configured (set CLICKHOUSE_URL)"
        })));
    };

    let limit = params.limit.unwrap_or(100).min(1000);

    // Allowlist action values
    const VALID_ACTIONS: &[&str] = &["query", "ingest", "deanonymize", "delete"];
    let action_filter = if let Some(a) = params.action.as_deref() {
        if !VALID_ACTIONS.contains(&a) {
            return Err(ApiError::Validation(format!(
                "invalid action '{}'; allowed: query, ingest, deanonymize, delete", a
            )));
        }
        format!(" AND action = '{a}'")
    } else {
        String::new()
    };

    // tenant_id from JWT (safe to interpolate — validated by middleware)
    let safe_tenant = auth.tenant_id.replace('\'', "''");
    let tenant_filter = format!(" AND tenant_id = '{safe_tenant}'");

    // doc_id must parse as UUID before interpolation
    let doc_filter = if let Some(ref id) = params.doc_id {
        uuid::Uuid::parse_str(id)
            .map_err(|_| ApiError::Validation("invalid doc_id: must be a UUID".to_string()))?;
        format!(" AND document_id = '{id}'")
    } else {
        String::new()
    };

    let query = format!(
        "SELECT document_id, action, pii_count_before, pii_count_after, \
         ner_degraded, processing_time_ms, legal_basis, user_id, model_version, \
         ai_act_risk_level, decision_explanation, ts_unix \
         FROM gdpr.gdpr_audit \
         WHERE 1=1{tenant_filter}{action_filter}{doc_filter} \
         ORDER BY ts_unix DESC \
         LIMIT {limit} \
         FORMAT JSONEachRow",
    );

    let url = format!(
        "{}/?query={}",
        ch.base_url().trim_end_matches('/'),
        urlencoding::encode(&query)
    );

    let resp = ch.http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let _ = resp.text().await;
        tracing::warn!(%status, "ClickHouse audit query failed");
        return Err(ApiError::Upstream(format!("ClickHouse error {status}")));
    }

    let body = resp.text().await.map_err(|e| ApiError::Upstream(e.to_string()))?;
    let events: Vec<AuditEvent> = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let total = events.len();
    Ok(Json(serde_json::json!({ "events": events, "total": total })))
}
```

Note: the handler signature now includes `Extension(auth): Extension<AuthContext>` — add the import at the top of `audit.rs`:
```rust
use axum::extract::{Extension, Query, State};
use crate::state::AuthContext;
```

- [ ] **Step 4: Run audit tests**

```bash
cd hacienda && cargo test -p gdpr-api test_audit 2>&1 | tail -10
```
Expected: all 3 pass.

- [ ] **Step 5: Check full compilation**

```bash
cd hacienda && cargo check -p gdpr-api 2>&1 | grep "error\[" | head -10
```
Expected: no errors. If the router calls `get_audit` without the `Extension`, it will fail — check `router.rs` and add `Extension` to the audit route if needed (see Step 6).

- [ ] **Step 6: Fix router if audit route lacks Extension extractor**

The `Extension<AuthContext>` extractor is populated by the JWT middleware. Ensure the audit route goes through the auth middleware layer. If `router.rs` has a separate `auth_routes` group, move the audit route there. Look for the audit handler registration in `router.rs` and verify it's in the authenticated route group (where `middleware::auth` runs).

```bash
grep -n "audit" hacienda/crates/gdpr-api/src/router.rs
```

If audit is in a public group, move it to the authenticated group.

- [ ] **Step 7: Run all gdpr-api tests**

```bash
cd hacienda && cargo test -p gdpr-api 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add hacienda/crates/gdpr-api/src/handlers/audit.rs hacienda/crates/gdpr-api/src/router.rs
git commit -m "feat(gdpr-api): audit handler with tenant_id scope and optional doc_id filter"
```

---

### Task 9: Run full test suite and verify clean build

- [ ] **Step 1: Run the full workspace test suite**

```bash
cd hacienda && cargo test 2>&1 | tail -30
```
Expected: all tests pass. No regressions in `gdpr-core`, `gdpr-api`, `gdpr-billing`.

- [ ] **Step 2: Build release binary**

```bash
cd hacienda && cargo build -p gdpr-api 2>&1 | tail -5
```
Expected: `Finished` with no warnings about unused imports in modified files.

- [ ] **Step 3: Commit final cleanup (if any warnings fixed)**

```bash
git add -p
git commit -m "chore(gdpr-api): fix any remaining compiler warnings from T6 implementation"
```

---
