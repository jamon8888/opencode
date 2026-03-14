use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::{
    audit::DocAuditDb,
    clients::clickhouse::ClickHouseClient,
    pii::{EnginePool, PiiEngine},
    resilience::CircuitBreaker,
};

/// Shared application state threaded through all MCP tool handlers.
pub struct AppState {
    pub pii_engine: Arc<Mutex<PiiEngine>>,
    pub db: Arc<Mutex<Connection>>,
    /// Entity-level audit map (which entities were found per document).
    pub doc_audit: DocAuditDb,
    /// Circuit breaker protecting kreuzberg document extraction calls.
    pub kreuzberg_cb: Arc<CircuitBreaker>,
    /// N-slot pool for the HTTP proxy path (concurrent anonymization).
    pub engine_pool: Arc<EnginePool>,
    /// ClickHouse audit trail client (GDPR Art. 30). None if CLICKHOUSE_URL unset.
    pub clickhouse: Option<Arc<ClickHouseClient>>,
}

impl AppState {
    /// Initialise state: open DB, create schema, load PII engine.
    pub fn new(pii_engine: PiiEngine, db: Connection) -> anyhow::Result<Self> {
        init_schema(&db)?;
        let db_arc = Arc::new(Mutex::new(db));

        // Pool size = num CPUs (regex + ONNX is CPU-bound; more slots than cores = contention)
        let pool_size   = num_cpus::get().max(2);
        let engine_pool = EnginePool::new(pii_engine.try_clone()?, pool_size)?;

        let clickhouse = std::env::var("CLICKHOUSE_URL").ok().map(|url| {
            tracing::info!(url = %url, "ClickHouse audit trail enabled");
            ClickHouseClient::new(&url)
        });

        Ok(Self {
            pii_engine: Arc::new(Mutex::new(pii_engine)),
            doc_audit: DocAuditDb::from_shared(Arc::clone(&db_arc)),
            db: db_arc,
            kreuzberg_cb: CircuitBreaker::new("kreuzberg", 5, Duration::from_secs(30)),
            engine_pool,
            clickhouse,
        })
    }
}

/// Create tables if they don't exist.
fn init_schema(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS documents (
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
        CREATE INDEX IF NOT EXISTS idx_dem_doc_id ON doc_entity_map(document_id);",
    )?;
    Ok(())
}
