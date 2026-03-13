use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::{pii::PiiEngine, resilience::CircuitBreaker};

/// Shared application state threaded through all MCP tool handlers.
pub struct AppState {
    pub pii_engine: Arc<Mutex<PiiEngine>>,
    pub db: Arc<Mutex<Connection>>,
    /// Circuit breaker protecting kreuzberg document extraction calls.
    pub kreuzberg_cb: Arc<CircuitBreaker>,
}

impl AppState {
    /// Initialise state: open DB, create schema, load PII engine.
    pub fn new(pii_engine: PiiEngine, db: Connection) -> anyhow::Result<Self> {
        init_schema(&db)?;
        Ok(Self {
            pii_engine: Arc::new(Mutex::new(pii_engine)),
            db: Arc::new(Mutex::new(db)),
            kreuzberg_cb: CircuitBreaker::new("kreuzberg"),
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
        );",
    )?;
    Ok(())
}
