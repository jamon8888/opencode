mod audit;
mod clients;
mod error;
mod extraction;
mod mcp;
mod pii;
mod resilience;
mod state;

use std::sync::Arc;

use rmcp::{service::ServiceExt, transport::io::stdio};

use crate::{mcp::GdprServer, pii::PiiEngine, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr) // keep stdout clean for MCP stdio transport
        .init();

    tracing::info!("gdpr-mcp starting");

    // Vault key is required — a zero-key fallback would be insecure in production.
    if std::env::var("CLOAKPIPE_VAULT_KEY").map(|k| k.is_empty()).unwrap_or(true) {
        anyhow::bail!("CLOAKPIPE_VAULT_KEY must be set to a non-empty secret");
    }

    let vault_path = std::env::var("GDPR_VAULT_PATH")
        .unwrap_or_else(|_| "gdpr_vault.db".to_string());
    let pii_engine = PiiEngine::load_for_test(&vault_path)?;

    // Document + audit store
    let db_path = std::env::var("GDPR_DB_PATH")
        .unwrap_or_else(|_| "gdpr_docs.db".to_string());
    let db = rusqlite::Connection::open(&db_path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    let state = Arc::new(AppState::new(pii_engine, db)?);
    let server = GdprServer::new(Arc::clone(&state));

    tracing::info!("gdpr-mcp ready — listening on stdio");

    let running = server.serve(stdio()).await?;
    running.waiting().await?;

    Ok(())
}
