use std::time::{SystemTime, UNIX_EPOCH};
use deadpool_sqlite::Pool;

// ── AuditLog (event log) ──────────────────────────────────────────────────────

#[derive(Clone)]
pub struct AuditLog {
    pool: Pool,
}

pub enum AuditEvent<'a> {
    Ingest { doc_id: &'a str, pii_count: usize },
    Search { query: &'a str },
    Delete { doc_id: &'a str },
    AuditQuery { doc_id: Option<&'a str> },
}

impl AuditLog {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub async fn record(&self, event: AuditEvent<'_>) -> anyhow::Result<()> {
        let ts = now_unix() as i64;
        let (event_type, doc_id, pii_count, detail): (String, Option<String>, Option<i64>, Option<String>) =
            match &event {
                AuditEvent::Ingest { doc_id, pii_count } => {
                    ("ingest".into(), Some(doc_id.to_string()), Some(*pii_count as i64), None)
                }
                AuditEvent::Search { query } => ("search".into(), None, None, Some(query.to_string())),
                AuditEvent::Delete { doc_id } => ("delete".into(), Some(doc_id.to_string()), None, None),
                AuditEvent::AuditQuery { doc_id } => {
                    ("audit_query".into(), doc_id.map(|s| s.to_string()), None, None)
                }
            };

        let conn = self.pool.get().await?;
        conn.interact(move |c| {
            c.execute(
                "INSERT INTO audit_log (event_type, doc_id, pii_count, detail, ts_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![event_type, doc_id, pii_count, detail, ts],
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow::anyhow!("audit insert: {e}"))?;
        Ok(())
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
    pool: Pool,
}

impl DocAuditDb {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    pub async fn record_entities(&self, doc_id: &str, rows: Vec<DocEntityRow>) -> anyhow::Result<()> {
        let conn = self.pool.get().await?;
        let doc_id = doc_id.to_string();
        conn.interact(move |c| {
            for row in &rows {
                c.execute(
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
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow::anyhow!("record_entities: {e}"))?;
        Ok(())
    }

    pub async fn entities_for_doc(&self, doc_id: &str) -> anyhow::Result<Vec<DocEntityRow>> {
        let conn = self.pool.get().await?;
        let doc_id = doc_id.to_string();
        let rows = conn.interact(move |c| {
            let mut stmt = c.prepare(
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
            Ok::<_, rusqlite::Error>(rows)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow::anyhow!("entities_for_doc: {e}"))?;
        Ok(rows)
    }

    pub async fn delete_document(&self, doc_id: &str) -> anyhow::Result<usize> {
        let conn = self.pool.get().await?;
        let doc_id = doc_id.to_string();
        let n = conn.interact(move |c| {
            c.execute(
                "DELETE FROM doc_entity_map WHERE document_id = ?1",
                rusqlite::params![doc_id],
            )
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow::anyhow!("delete_document: {e}"))?;
        Ok(n)
    }

    pub async fn list_documents(&self) -> anyhow::Result<Vec<DocumentSummary>> {
        let conn = self.pool.get().await?;
        let docs = conn.interact(|c| {
            let mut stmt = c.prepare(
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
            Ok::<_, rusqlite::Error>(docs)
        })
        .await
        .map_err(|e| anyhow::anyhow!("interact error: {e}"))?
        .map_err(|e| anyhow::anyhow!("list_documents: {e}"))?;
        Ok(docs)
    }
}
