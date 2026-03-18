# T6 — Qdrant Semantic Search + Tenant Isolation Design

**Date:** 2026-03-18
**Status:** Draft
**Scope:** `hacienda/crates/gdpr-api` + `hacienda/crates/gdpr-core` (new methods on `QdrantStore`)

---

## Goal

Wire Qdrant semantic search into `gdpr-api`, enforce per-tenant row isolation across all document tables and Qdrant points, and return `session_id` from `post_document` so callers can deanonymize document-scoped responses.

---

## Prerequisite

**T6 depends on T5 (`feat/v2-t5-thin-client`) being merged into `dev` first.** After T5 merges, `post_document` has a full implementation (anonymize via `anonymize_with_profile`, SQLite chunk storage, ClickHouse audit). If T6 is implemented before T5 merges, the `post_document` stub must be fully implemented from scratch as part of T6, following the pattern in `gdpr-api/src/handlers/anonymize.rs:62-116`.

---

## Context

After T5, `gdpr-mcp` is a thin HTTP client — all document business logic lives in `gdpr-api`. T5 deferred:

1. `vec_store: Option<Arc<VecStore>>` in `AppState` — a placeholder stub, nothing wired
2. No `tenant_id` column on `documents`, `doc_chunks`, `doc_entity_map` — cross-tenant isolation not enforced
3. `IngestResp` does not return `session_id`
4. `AuditParams.doc_id` filter removed in T5 as T6 deferral

`gdpr-core` already has a fully-implemented `QdrantStore` (`crates/gdpr-core/src/clients/qdrant.rs`) with `upsert_chunks`, `search`, and `delete` methods. T6 adds two new methods to `QdrantStore`: `upsert_chunks_tenant` and `search_tenant`, which accept a `tenant_id` parameter and store/filter by it in Qdrant point payloads.

---

## Architecture

```
POST /v1/documents
  → anonymize_with_profile  (gdpr-core, unchanged)
  → generate session_id, insert into session_cache (manual, see session_id section)
  → INSERT documents        (SQLite, + tenant_id)
  → INSERT doc_chunks       (SQLite, + tenant_id)
  → INSERT doc_entity_map   (SQLite, + tenant_id)
  → write_audit_row         (ClickHouse, unchanged)
  → tokio::spawn: QdrantStore::upsert_chunks_tenant (fire-and-forget)
  ← IngestResp { doc_id, session_id, pii_count, ner_degraded, ai_act_risk_level,
                 chunk_count, decision_explanation }

POST /v1/search
  → if state.qdrant is Some:
      QdrantStore::search_tenant(query, tenant_id, limit) → Vec<QdrantHit>
  → else:
      SQLite LIKE (existing fallback, + tenant_id filter)
  ← SearchResp

DELETE /v1/documents/:id
  → verify doc belongs to tenant  (SELECT WHERE id AND tenant_id)
  → DELETE doc_chunks WHERE doc_id AND tenant_id
  → DELETE doc_entity_map WHERE document_id AND tenant_id
  → DELETE documents WHERE id AND tenant_id
  → tokio::spawn: QdrantStore::delete(doc_id) (fire-and-forget)
  ← 204 or 404

GET /v1/documents       → WHERE tenant_id = ?
GET /v1/review-queue    → WHERE tenant_id = ?
GET /v1/audit           → WHERE tenant_id = ? [AND document_id = ?]
```

---

## File Map

| File | Change |
|------|--------|
| `gdpr-core/src/clients/qdrant.rs` | Add `upsert_chunks_tenant` and `search_tenant` methods (see below) |
| `gdpr-api/src/main.rs` | Add `tenant_id` `ALTER TABLE` migrations; init `QdrantStore::from_env()` |
| `gdpr-api/src/state.rs` | Replace `pub struct VecStore;` stub + `vec_store` field with `qdrant: Option<Arc<QdrantStore>>` |
| `gdpr-api/src/handlers/documents.rs` | `tenant_id` on all inserts/selects/deletes; Qdrant fire-and-forget; `session_id` in `IngestResp`; full `post_document` body if T5 not yet merged |
| `gdpr-api/src/handlers/search.rs` | Qdrant-first with LIKE fallback; `tenant_id` filter on both paths |
| `gdpr-api/src/handlers/audit.rs` | Add `tenant_id` WHERE clause; restore `doc_id` optional filter |
| `gdpr-api/Cargo.toml` | No new deps — `gdpr-core` already declared |

---

## New Methods on QdrantStore (`gdpr-core/src/clients/qdrant.rs`)

### `upsert_chunks_tenant`

```rust
/// Embed and upsert multiple text chunks, storing `tenant_id` in each point payload.
/// Point IDs are deterministic UUID v5 from `"{tenant_id}/{doc_id}/{chunk_idx}"`.
pub async fn upsert_chunks_tenant(
    &self,
    doc_id:    &str,
    tenant_id: &str,
    chunks:    &[(usize, String)],  // (chunk_idx, chunk_text)
) -> anyhow::Result<()>
```

**Point ID computation:**
```rust
use uuid::{Uuid, Version};
let ns = Uuid::NAMESPACE_URL;
let point_id = Uuid::new_v5(&ns, format!("{tenant_id}/{doc_id}/{chunk_idx}").as_bytes());
```

**Payload per point:**
```json
{
  "doc_id":     "...",
  "tenant_id":  "...",
  "chunk_idx":  0,
  "chunk_text": "..."
}
```

**Upsert body sent to Qdrant** (`PUT /collections/{collection}/points`):
```json
{
  "points": [
    { "id": "<uuid_v5>", "vector": [...], "payload": { ... } }
  ]
}
```

### `search_tenant`

```rust
/// Embed query, search Qdrant filtered by tenant_id.
/// Returns at most `limit` hits ordered by score descending.
pub async fn search_tenant(
    &self,
    query:     &str,
    tenant_id: &str,
    limit:     u64,
) -> anyhow::Result<Vec<QdrantHit>>
```

**Filter sent to Qdrant** (`POST /collections/{collection}/points/search`):
```json
{
  "vector": [...],
  "filter": {
    "must": [{ "key": "tenant_id", "match": { "value": "<tenant_id>" } }]
  },
  "limit": 10,
  "with_payload": true
}
```

**Return type** — use existing `QdrantHit` (already in `qdrant.rs`):
```rust
pub struct QdrantHit {
    pub doc_id:     String,
    pub score:      f32,
    pub chunk_text: Option<String>,
    pub chunk_idx:  Option<usize>,
}
```

---

## Schema Migrations

Add as individual `conn.execute()` calls in `main.rs`, **not** inside `execute_batch`. SQLite does not support `IF NOT EXISTS` on `ALTER TABLE ADD COLUMN` — each call must catch the "duplicate column name" error and skip silently:

```rust
let alter_stmts = [
    "ALTER TABLE documents      ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE doc_chunks     ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE doc_entity_map ADD COLUMN tenant_id TEXT NOT NULL DEFAULT ''",
    "CREATE INDEX IF NOT EXISTS idx_docs_tenant    ON documents(tenant_id)",
    "CREATE INDEX IF NOT EXISTS idx_chunks_tenant  ON doc_chunks(tenant_id)",
    "CREATE INDEX IF NOT EXISTS idx_dem_tenant     ON doc_entity_map(tenant_id)",
];
for stmt in &alter_stmts {
    if let Err(e) = conn.execute(stmt, []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e.into());
        }
    }
}
```

`DEFAULT ''` ensures zero-downtime compatibility with any pre-existing rows.

---

## AppState Change

**Remove** from `state.rs`:
```rust
/// Placeholder for optional vector store. Real implementation comes in T6/T7.
pub struct VecStore;
```
and the field `pub vec_store: Option<Arc<VecStore>>`.

**Add**:
```rust
pub qdrant: Option<Arc<gdpr_core::clients::qdrant::QdrantStore>>,
```

In `main.rs`, initialize:
```rust
qdrant: gdpr_core::clients::qdrant::QdrantStore::from_env(),
```
`from_env()` returns `None` if `QDRANT_URL` is unset — service starts without Qdrant and falls back to LIKE search.

---

## IngestResp

Add `session_id`, `chunk_count`, `decision_explanation`. Keep `ai_act_risk_level`.

```rust
// Before (current):
pub struct IngestResp {
    pub doc_id:            String,
    pub pii_count:         usize,
    pub ner_degraded:      bool,
    pub ai_act_risk_level: String,
}

// After (T6):
pub struct IngestResp {
    pub doc_id:               String,
    pub session_id:           String,   // NEW — for deanonymize calls
    pub pii_count:            usize,
    pub ner_degraded:         bool,
    pub ai_act_risk_level:    String,   // KEPT
    pub chunk_count:          usize,    // NEW
    pub decision_explanation: String,   // NEW
}
```

---

## session_id in post_document

`anonymize_with_profile` is a pure function — it returns `ProfileAnonymizeResult` with `token_map: HashMap<String, String>` but does NOT insert into `session_cache`. The handler must do this manually, following the exact pattern in `handlers/anonymize.rs:62-93`:

```rust
// 1. Generate session_id
let session_id = uuid::Uuid::new_v4().to_string();

// 2. Build session context and engine, then call anonymize_with_profile
// Follow the exact pattern in handlers/anonymize.rs:62-78:
//   let engine = state.engine_pool.get().await?;
//   let mut session_ctx = SessionContext::new(profile.clone());
//   let result = anonymize_with_profile(&text, profile, &mut session_ctx, &engine)?;

// 3. Insert token_map into session_cache
let sc = SessionCache {
    token_map: {
        let dm = DashMap::new();
        for (pseudo, original) in &result.token_map {
            dm.insert(pseudo.clone(), original.clone());
        }
        dm
    },
    created_at: std::time::Instant::now(),
};
state.session_cache.insert(session_id.clone(), sc);

// 4. Return session_id in IngestResp
```

---

## Qdrant Integration

### Upsert (post_document)

After SQLite writes succeed, fire-and-forget:

```rust
if let Some(ref q) = state.qdrant {
    let q = Arc::clone(q);
    let chunks: Vec<(usize, String)> = chunk_rows
        .iter()
        .map(|(idx, text, _offset)| (*idx, text.clone()))
        .collect();
    let doc_id2    = doc_id.clone();
    let tenant_id2 = auth.tenant_id.clone();
    tokio::spawn(async move {
        if let Err(e) = q.upsert_chunks_tenant(&doc_id2, &tenant_id2, &chunks).await {
            tracing::warn!(error = %e, doc_id = %doc_id2, "qdrant upsert failed");
        }
    });
}
```

Qdrant failure does not fail the ingest — SQLite is the source of truth.

### Search

```rust
if let Some(ref q) = state.qdrant {
    let hits = q.search_tenant(&req.query, &auth.tenant_id, limit as u64).await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let results = hits.into_iter().map(|h| SearchResult {
        doc_id: h.doc_id,
        chunk:  h.chunk_text.unwrap_or_default(),
        score:  h.score as f64,
    }).collect();
    return Ok(Json(SearchResp { results }));
}
// fallback: LIKE query (unchanged, but add WHERE tenant_id = ?1)
```

Qdrant search failure returns `ApiError::Internal` — do NOT silently fall back (wrong results are worse than an error).

### Delete

```rust
if let Some(ref q) = state.qdrant {
    let q = Arc::clone(q);
    let doc_id2 = doc_id.clone();
    tokio::spawn(async move {
        if let Err(e) = q.delete(&doc_id2).await {
            tracing::warn!(error = %e, doc_id = %doc_id2, "qdrant delete failed");
        }
    });
}
```

---

## Tenant Isolation Enforcement

Every handler receives `auth: Extension<AuthContext>` (populated by JWT middleware) with `tenant_id: String`. Enforcement is at the SQL/Qdrant query level only.

**list_documents:**
```sql
SELECT d.id AS doc_id, d.created_at, COUNT(e.id) AS entity_count
FROM documents d
LEFT JOIN doc_entity_map e ON e.document_id = d.id
WHERE d.tenant_id = ?1
GROUP BY d.id
ORDER BY d.created_at DESC
```

**delete_document** (404 guard):
```sql
SELECT id FROM documents WHERE id = ?1 AND tenant_id = ?2
```
If no row → `ApiError::NotFound`.

Cascade deletes:
```sql
DELETE FROM doc_chunks     WHERE doc_id      = ?1 AND tenant_id = ?2;
DELETE FROM doc_entity_map WHERE document_id = ?1 AND tenant_id = ?2;
DELETE FROM documents      WHERE id          = ?1 AND tenant_id = ?2;
```

**review_queue:**
```sql
SELECT d.id AS doc_id, COUNT(e.id) AS entity_count, d.created_at
FROM documents d
JOIN doc_entity_map e ON e.document_id = d.id
WHERE d.tenant_id = ?1
GROUP BY d.id
HAVING COUNT(e.id) >= ?2
ORDER BY entity_count DESC
LIMIT ?3
```

---

## Audit Handler

Restore `doc_id` optional filter (removed in T5):

```rust
#[derive(Deserialize)]
pub struct AuditQuery {
    pub doc_id: Option<String>,
    pub limit:  Option<u32>,
}
```

ClickHouse query (add `tenant_id` filter and optional `doc_id`):
```sql
SELECT * FROM audit_log
WHERE tenant_id = ?
  AND (? = '' OR document_id = ?)
ORDER BY created_at DESC
LIMIT ?
```

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Qdrant upsert fails | `tracing::warn!`, ingest still returns 201 (SQLite is source of truth) |
| Qdrant search fails | `ApiError::Internal` — do NOT fall back silently |
| Qdrant delete fails | `tracing::warn!`, document deleted from SQLite (orphaned Qdrant point unreachable after row gone) |
| `QDRANT_URL` not set | `state.qdrant = None`; search uses LIKE |
| Wrong tenant on delete | 404 `ApiError::NotFound` |
| ALTER TABLE duplicate column | Catch "duplicate column name" in error string, skip silently |

---

## Testing

All `gdpr-api` tests use in-memory SQLite (`:memory:`). Qdrant calls mocked with `wiremock`.

| Test | Covers |
|------|--------|
| `test_post_document_stores_tenant_id` | `tenant_id` persisted in all three tables |
| `test_tenant_isolation_list` | Tenant A cannot see Tenant B's documents |
| `test_tenant_isolation_delete` | Delete with wrong `tenant_id` → 404 |
| `test_search_falls_back_to_like_when_no_qdrant` | `state.qdrant = None`, LIKE search returns results |
| `test_search_uses_qdrant_when_available` | wiremock Qdrant server, assert `tenant_id` filter in request body |
| `test_ingest_resp_has_session_id` | `IngestResp.session_id` is a non-empty UUID string |
| `test_audit_doc_id_filter` | `?doc_id=x` narrows results correctly |
| `test_qdrant_upsert_failure_does_not_fail_ingest` | wiremock returns 500 on upsert, ingest still returns 201 |
| `test_upsert_chunks_tenant_point_ids_are_deterministic` | Same input → same UUID v5 point IDs (in gdpr-core tests) |
| `test_search_tenant_sends_filter` | `search_tenant` request body contains correct `filter` (in gdpr-core tests) |

---

## Environment Variables

| Variable | Purpose | Required |
|----------|---------|----------|
| `QDRANT_URL` | Qdrant base URL (e.g. `http://localhost:6333`) | No — falls back to LIKE |
| `QDRANT_COLLECTION` | Collection name (default: `gdpr_docs`) | No |
| `EMBEDDING_URL` | OpenAI-compatible embeddings endpoint | Required if `QDRANT_URL` set |
| `EMBEDDING_MODEL` | Model name (default: `nomic-embed-text`) | No |

---

## Out of Scope (T7+)

- `session_cache` persistence across restarts (currently in-memory DashMap)
- Cross-tenant admin queries
- Qdrant collection sharding / multi-node setup
- Re-indexing existing documents after migration (documents ingested before T6 have `tenant_id = ''`)
