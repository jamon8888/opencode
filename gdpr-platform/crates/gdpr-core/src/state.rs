use std::sync::Arc;
use anyhow::Result;
use deadpool_sqlite::Pool;
use parking_lot::Mutex;

use crate::pii::engine::PiiEngine;
use crate::pii::pool::EnginePool;
use crate::audit::DocAuditDb;
use crate::clients::clickhouse::ClickHouseClient;
use crate::clients::vec_store::VecStore;
use crate::resilience::CircuitBreaker;

/// Shared application state threaded through all handlers.
pub struct CoreState {
    pub pii_engine:   Arc<Mutex<PiiEngine>>,
    pub db:           Pool,
    /// Entity-level audit map (which entities were found per document).
    pub doc_audit:    DocAuditDb,
    /// Circuit breaker protecting kreuzberg document extraction calls.
    pub kreuzberg_cb: Arc<CircuitBreaker>,
    /// N-slot pool for the HTTP proxy path (concurrent anonymization).
    pub engine_pool:  Arc<EnginePool>,
    /// ClickHouse audit trail client (GDPR Art. 30). None if CLICKHOUSE_URL unset.
    pub clickhouse:   Option<Arc<ClickHouseClient>>,
    /// SQLite-backed in-process vector store. None if EMBEDDING_URL unset.
    pub vec_store:    Option<Arc<VecStore>>,
}

impl CoreState {
    pub async fn new(pii_engine: PiiEngine, db_path: &str) -> Result<Arc<Self>> {
        let cfg  = deadpool_sqlite::Config::new(db_path);
        let pool = cfg.create_pool(deadpool_sqlite::Runtime::Tokio1)?;
        {
            let conn = pool.get().await?;
            conn.interact(|c| {
                c.execute_batch(
                    "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;"
                )?;
                init_schema(c)
            }).await
            .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
            .map_err(|e| anyhow::anyhow!("schema init: {e}"))?;
        }

        // Pool size = num CPUs (regex + ONNX is CPU-bound; more slots than cores = contention)
        let pool_size   = num_cpus::get().max(2);
        let engine_pool = Arc::new(EnginePool::new(pool_size, pii_engine.try_clone()?)?);
        let pii_engine  = Arc::new(Mutex::new(pii_engine));

        let clickhouse = std::env::var("CLICKHOUSE_URL").ok().map(|url| {
            tracing::info!(url = %url, "ClickHouse audit trail enabled");
            Arc::new(ClickHouseClient::new(&url))
        });

        let vec_store = std::env::var("EMBEDDING_URL").ok().map(|url| {
            let model = std::env::var("EMBEDDING_MODEL")
                .unwrap_or_else(|_| "nomic-embed-text".into());
            tracing::info!(url = %url, model = %model, "VecStore (deadpool-sqlite-backed) enabled");
            Arc::new(VecStore::new(pool.clone()))
        });

        let doc_audit = DocAuditDb::new(pool.clone());
        let kreuzberg_cb = CircuitBreaker::new("kreuzberg", 5, std::time::Duration::from_secs(30));

        Ok(Arc::new(Self {
            pii_engine,
            db: pool,
            doc_audit,
            kreuzberg_cb,
            engine_pool,
            clickhouse,
            vec_store,
        }))
    }
}

/// Create tables if they don't exist.
fn init_schema(c: &rusqlite::Connection) -> rusqlite::Result<()> {
    c.execute_batch("
        CREATE TABLE IF NOT EXISTS documents (
            id           TEXT PRIMARY KEY,
            anon_text    TEXT NOT NULL,
            pii_count    INTEGER NOT NULL,
            ner_degraded INTEGER NOT NULL,
            created_at   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type   TEXT NOT NULL,
            doc_id       TEXT,
            pii_count    INTEGER,
            detail       TEXT,
            ts_unix      INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS doc_entity_map (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            document_id     TEXT NOT NULL,
            entity_type     TEXT NOT NULL,
            pseudonym       TEXT NOT NULL,
            detection_layer TEXT NOT NULL,
            confidence      REAL,
            ner_degraded    INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_dem_doc_id ON doc_entity_map(document_id);

        CREATE TABLE IF NOT EXISTS doc_chunks (
            id           TEXT PRIMARY KEY,
            doc_id       TEXT NOT NULL,
            chunk_idx    INTEGER NOT NULL,
            chunk_text   TEXT NOT NULL,
            chunk_offset INTEGER NOT NULL,
            created_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_dc_doc_id ON doc_chunks(doc_id);

        CREATE TABLE IF NOT EXISTS vec_chunks (
            chunk_id  TEXT PRIMARY KEY,
            embedding BLOB NOT NULL
        );
    ")
}
