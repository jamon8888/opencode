mod audit;
mod clients;
mod error;
mod extraction;
mod mcp;
mod ner;
mod pii;
mod proxy;
mod resilience;
mod state;

use std::sync::Arc;

use axum::{routing::post, Router};
use rmcp::{service::ServiceExt, transport::io::stdio};

use crate::{mcp::GdprServer, pii::PiiEngine, proxy::ProxyState, state::AppState};

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
    let db_path = std::env::var("GDPR_DB_PATH")
        .unwrap_or_else(|_| "gdpr_docs.db".to_string());
    let model_dir = std::env::var("GLINER_MODEL_DIR")
        .unwrap_or_else(|_| "models/gliner-pii-edge".to_string());

    // load_production: L1 always, L2 if model present (Invariant I2)
    let pii_engine = PiiEngine::load_production(&vault_path, &model_dir)?;

    let db = rusqlite::Connection::open(&db_path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

    let state = Arc::new(AppState::new(pii_engine, db)?);
    let server = GdprServer::new(Arc::clone(&state));

    // ── HTTP anonymization proxy (:8080) ──────────────────────────────────────
    let proxy_addr = std::env::var("PROXY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let proxy_state = Arc::new(ProxyState {
        engine_pool:  Arc::clone(&state.engine_pool),
        http_client:  reqwest::Client::new(),
        upstream_url: std::env::var("TENSORZERO_URL")
            .unwrap_or_else(|_| "http://localhost:3000/openai/v1".to_string()),
        upstream_key: std::env::var("TENSORZERO_KEY")
            .unwrap_or_else(|_| "hacienda".to_string()),
    });
    let app = Router::new()
        .route("/openai/v1/chat/completions", post(proxy::chat_completions))
        .with_state(proxy_state);
    let listener = tokio::net::TcpListener::bind(&proxy_addr).await?;
    tracing::info!(%proxy_addr, "anonymization proxy listening");

    // ── MCP stdio server ──────────────────────────────────────────────────────
    tracing::info!("gdpr-mcp ready — listening on stdio");
    let running = server.serve(stdio()).await?;

    // Run both concurrently — select! exits when either finishes
    tokio::select! {
        result = axum::serve(listener, app) => {
            result?;
        }
        result = running.waiting() => {
            result?;
        }
    }

    Ok(())
}
