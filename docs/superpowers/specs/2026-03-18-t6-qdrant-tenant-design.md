# T6 — Qdrant Semantic Search + Tenant Isolation Design

**Date:** 2026-03-18
**Status:** Draft
**Scope:** `hacienda/crates/gdpr-api` only — `gdpr-mcp` unchanged (thin client)

---

## Goal

Wire Qdrant semantic search into `gdpr-api`, enforce per-tenant row isolation across all document tables and Qdrant points, and return `session_id` from `post_document` so callers can deanonymize document-scoped responses.

---

## Context

After T5, `gdpr-mcp` is a thin HTTP client — all document business logic lives in `gdpr-api`. T5 left three items explicitly deferred:

1. `vec_store: None` in `AppState` (placeholder stub, no Qdrant wiring)
2. No `tenant_id` column on `documents`, `doc_chunks`, `doc_entity_map` — cross-tenant isolation not enforced
3. `IngestResp` does not return `session_id` — callers cannot deanonymize document responses
4. `AuditParams.doc_id` filter removed in T5 as T6 deferral

`gdpr-core` already contains a fully-implemented `QdrantStore` (`crates/gdpr-core/src/clients/qdrant.rs`) with `upsert_chunks`, `search`, and `delete` methods. `gdpr-api` already depends on `gdpr-core`. No new crate dependencies are needed.

---

## Architecture

All changes are in `gdpr-api`. The thin client (`gdpr-mcp`) calls the same REST endpoints — it does not need modification.

```
POST /v1/documents
  → anonymize_with_profile  (gdpr-core, unchanged)
  → INSERT documents        (SQLite, + tenant_id)
  → INSERT doc_chunks       (SQLite, + tenant_id)
  → INSERT doc_entity_map   (SQLite, + tenant_id)
  → write_audit_row         (ClickHouse, unchanged)
  → tokio::spawn: QdrantStore::upsert_chunks (fire-and-forget)
  ← IngestResp { doc_id, session_id, pii_count, ner_degraded, chunk_count, decision_explanation }

POST /v1/search
  → if state.qdrant is Some:
      QdrantStore::search_tenant(query, tenant_id, limit)
  → else:
      SQLite LIKE (existing fallback, + tenant_id filter)
  ← SearchResp

DELETE /v1/documents/:id
  → verify doc belongs to tenant  (tenant_id guard)
  → DELETE doc_chunks WHERE doc_id AND tenant_id
  → DELETE doc_entity_map WHERE document_id AND tenant_id
  → DELETE documents WHERE id AND tenant_id
  → tokio::spawn: QdrantStore::delete(doc_id) (fire-and-forget)
  ← 204 or 404

GET /v1/documents
  → SELECT ... WHERE tenant_id = ?

GET /v1/audit
  → ClickHouse query WHERE tenant_id = ? [AND document_id = ?]

GET /v1/review-queue
  → SELECT ... WHERE tenant_id = ?
```

---

## File Map

| File | Change |
|------|--------|
| `src/main.rs` | Add `tenant_id` `ALTER TABLE` migrations; init `QdrantStore::from_env()` into `AppState` |
| `src/state.rs` | Replace `pub struct VecStore;` stub + `vec_store` field with `qdrant: Option<Arc<QdrantStore>>` |
| `src/handlers/documents.rs` | `tenant_id` on all inserts/selects/deletes; Qdrant fire-and-forget upsert + delete; `session_id` in `IngestResp` |
| `src/handlers/search.rs` | Qdrant-first with LIKE fallback; tenant_id filter on both paths |
| `src/handlers/audit.rs` | Add `tenant_id` WHERE clause; restore `doc_id` optional filter |
| `src/handlers/mod.rs` | No change (just re-exports) |
| `Cargo.toml` | No new deps — `gdpr-core` already declared |

---

## Schema Migrations

Run as `ALTER TABLE` statements in the existing `execute_batch` block in `main.rs`, after the `CREATE TABLE IF NOT EXISTS` block. `DEFAULT ''` ensures zero-downtime compatibility with any pre-existing rows.

```sql
ALTER TABLE documents      ADD COLUMN tenant_id TEXT NOT NULL DEFAULT '';
ALTER TABLE doc_chunks     ADD COLUMN tenant_id TEXT NOT NULL DEFAULT '';
ALTER TABLE doc_entity_map ADD COLUMN tenant_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_docs_tenant ON documents(tenant_id);
CREATE INDEX IF NOT EXISTS idx_chunks_tenant ON doc_chunks(tenant_id);
CREATE INDEX IF NOT EXISTS idx_dem_tenant ON doc_entity_map(tenant_id);
```

`ALTER TABLE ... ADD COLUMN` is idempotent in SQLite when the column already exists — wrap each in a `conn.execute` with `IGNORE` error handling, or check for "duplicate column" error and skip.

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
`from_env()` returns `None` if `QDRANT_URL` is unset — the service starts without Qdrant and falls back to LIKE search.

---

## IngestResp

Add `session_id` field. The session key is already created during `anonymize_with_profile` and stored in `state.session_cache`. Return it in the response so `gdpr-mcp` (and any direct API caller) can later call `/v1/deanonymize` with that session.

```rust
pub struct IngestResp {
    pub doc_id:               String,
    pub session_id:           String,   // NEW
    pub pii_count:            usize,
    pub ner_degraded:         bool,
    pub ai_act_risk_level:    String,
    pub chunk_count:          usize,
    pub decision_explanation: String,
}
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
    let doc_id2 = doc_id.clone();
    let tenant_id2 = auth.tenant_id.clone();
    tokio::spawn(async move {
        if let Err(e) = q.upsert_chunks_tenant(&doc_id2, &tenant_id2, chunks).await {
            tracing::warn!(error = %e, doc_id = %doc_id2, "qdrant upsert failed");
        }
    });
}
```

Qdrant failure does not fail the ingest — SQLite is the source of truth. The warn log is observable.

### Point ID scheme

`QdrantStore::upsert_chunks_tenant` computes point IDs as:
```
uuid_v5(NAMESPACE_URL, "{tenant_id}/{doc_id}/{chunk_idx}")
```
Deterministic — re-ingesting the same document replaces existing points (upsert semantics).

### Payload shape

Each Qdrant point payload:
```json
{
  "doc_id":    "...",
  "tenant_id": "...",
  "chunk_idx": 0,
  "chunk_text": "..."
}
```

### Search

```rust
pub async fn search_tenant(
    &self,
    query: &str,
    tenant_id: &str,
    limit: u32,
) -> Result<Vec<VecHit>, QdrantError>
```

Filter sent to Qdrant:
```json
{
  "filter": {
    "must": [{ "key": "tenant_id", "match": { "value": "<tenant_id>" } }]
  },
  "limit": <limit>
}
```

`VecHit` already defined in `gdpr-core`: `{ doc_id: String, chunk: String, score: f64 }`.

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

Every handler receives `AuthContext` (populated by JWT middleware, contains `tenant_id: String`). Enforcement is purely at the SQL/Qdrant query level — no application-layer filtering.

**list_documents:**
```sql
SELECT d.id AS doc_id, d.created_at, COUNT(e.id) AS entity_count
FROM documents d
LEFT JOIN doc_entity_map e ON e.document_id = d.id
WHERE d.tenant_id = ?1
GROUP BY d.id
ORDER BY d.created_at DESC
```

**delete_document** (404 if wrong tenant):
```sql
SELECT id FROM documents WHERE id = ?1 AND tenant_id = ?2
```
If no row found → `ApiError::NotFound`.

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

Restore `doc_id` optional filter (removed in T5 as T6 deferral):

```rust
pub struct ReviewQueueQuery {
    pub threshold: Option<u32>,
    pub limit:     Option<u32>,
}

pub struct AuditQuery {
    pub doc_id: Option<String>,   // restored
    pub limit:  Option<u32>,
}
```

ClickHouse query:
```sql
SELECT * FROM audit_log
WHERE tenant_id = ?
  AND (? IS NULL OR document_id = ?)
ORDER BY created_at DESC
LIMIT ?
```

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Qdrant upsert fails | Warn log, ingest still succeeds (SQLite is source of truth) |
| Qdrant search fails | Return `ApiError::Internal`, do NOT silently fall back (client sees an error, not wrong results) |
| Qdrant delete fails | Warn log, document deleted from SQLite (orphaned Qdrant point — acceptable, point is unreachable after tenant row gone) |
| `QDRANT_URL` not set | `state.qdrant = None`; search falls back to LIKE |
| Wrong tenant on delete | 404 `ApiError::NotFound` |
| ALTER TABLE duplicate column | Catch "duplicate column name" SQLite error, skip silently |

---

## Testing

All tests in `gdpr-api` using in-memory SQLite (`":memory:"`). Qdrant calls mocked with `wiremock`.

| Test | Covers |
|------|--------|
| `test_post_document_stores_tenant_id` | `tenant_id` persisted in `documents`, `doc_chunks`, `doc_entity_map` |
| `test_tenant_isolation_list` | Tenant A cannot see Tenant B's documents |
| `test_tenant_isolation_delete` | Delete with wrong `tenant_id` returns 404 |
| `test_search_falls_back_to_like_when_no_qdrant` | `state.qdrant = None`, LIKE search still works |
| `test_search_uses_qdrant_when_available` | wiremock Qdrant server, assert `tenant_id` filter in payload |
| `test_ingest_resp_has_session_id` | `IngestResp.session_id` is non-empty after ingest |
| `test_audit_doc_id_filter` | `?doc_id=x` filters ClickHouse results |
| `test_qdrant_upsert_failure_does_not_fail_ingest` | wiremock returns 500, ingest still returns 201 |

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

- `session_cache` persistence (currently in-memory DashMap — survives restarts only within process lifetime)
- Cross-tenant admin queries
- Qdrant collection sharding / multi-node setup
- Re-indexing existing documents after migration
