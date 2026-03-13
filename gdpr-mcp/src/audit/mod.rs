use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

// ── AuditLog (event log) ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuditLog {
    db: Arc<Mutex<Connection>>,
}

pub enum AuditEvent<'a> {
    Ingest { doc_id: &'a str, pii_count: usize },
    Search { query: &'a str },
    Delete { doc_id: &'a str },
    AuditQuery { doc_id: Option<&'a str> },
}

impl AuditLog {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { db }
    }

    pub fn record(&self, event: AuditEvent<'_>) {
        let ts = now_unix() as i64;
        let (event_type, doc_id, pii_count, detail): (&str, Option<&str>, Option<i64>, Option<&str>) =
            match &event {
                AuditEvent::Ingest { doc_id, pii_count } => {
                    ("ingest", Some(doc_id), Some(*pii_count as i64), None)
                }
                AuditEvent::Search { query } => ("search", None, None, Some(query)),
                AuditEvent::Delete { doc_id } => ("delete", Some(doc_id), None, None),
                AuditEvent::AuditQuery { doc_id } => ("audit_query", *doc_id, None, None),
            };

        if let Ok(db) = self.db.lock() {
            let _ = db.execute(
                "INSERT INTO audit_log (event_type, doc_id, pii_count, detail, ts_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![event_type, doc_id, pii_count, detail, ts],
            );
        }
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── DocAuditDb (entity map) ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DocEntityRow {
    pub entity_type: String,
    pub pseudonym: String,
    pub detection_layer: String,
    pub confidence: Option<f64>,
    pub ner_degraded: bool,
}

#[derive(Debug, Clone)]
pub struct DocumentSummary {
    pub document_id: String,
    pub entity_count: usize,
}

pub struct DocAuditDb {
    conn: Arc<Mutex<Connection>>,
}

impl DocAuditDb {
    /// Open a standalone DocAuditDb (used in unit tests).
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Wrap an existing shared connection (used in AppState).
    pub fn from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub(crate) fn init_schema(conn: &Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS doc_entity_map (
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

    pub fn record_entities(&self, doc_id: &str, rows: &[DocEntityRow]) -> anyhow::Result<()> {
        let db = self.conn.lock().unwrap();
        for row in rows {
            db.execute(
                "INSERT INTO doc_entity_map
                 (document_id, entity_type, pseudonym, detection_layer, confidence, ner_degraded)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    doc_id,
                    row.entity_type,
                    row.pseudonym,
                    row.detection_layer,
                    row.confidence,
                    row.ner_degraded as i32
                ],
            )?;
        }
        Ok(())
    }

    pub fn entities_for_doc(&self, doc_id: &str) -> anyhow::Result<Vec<DocEntityRow>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT entity_type, pseudonym, detection_layer, confidence, ner_degraded
             FROM doc_entity_map WHERE document_id = ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![doc_id], |r| {
                Ok(DocEntityRow {
                    entity_type: r.get(0)?,
                    pseudonym: r.get(1)?,
                    detection_layer: r.get(2)?,
                    confidence: r.get(3)?,
                    ner_degraded: r.get::<_, i32>(4)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_document(&self, doc_id: &str) -> anyhow::Result<usize> {
        let db = self.conn.lock().unwrap();
        let n = db.execute(
            "DELETE FROM doc_entity_map WHERE document_id = ?1",
            rusqlite::params![doc_id],
        )?;
        Ok(n)
    }

    pub fn list_documents(&self) -> anyhow::Result<Vec<DocumentSummary>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT document_id, COUNT(*) FROM doc_entity_map GROUP BY document_id",
        )?;
        let docs = stmt
            .query_map([], |r| {
                Ok(DocumentSummary {
                    document_id: r.get(0)?,
                    entity_count: r.get::<_, i64>(1)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(docs)
    }
}
