use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

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
