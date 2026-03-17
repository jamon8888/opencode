mod api_client;
mod mcp;
mod proxy;

use std::sync::Arc;

use axum::{routing::post, Router};
use rmcp::service::ServiceExt;
use rmcp::transport::io::stdio;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr) // keep stdout clean for MCP stdio transport
        .init();

    tracing::info!("gdpr-mcp starting");

    // Build ApiClient (thin-client: calls gdpr-api over HTTP) — T5
    let api_client = Arc::new(api_client::ApiClient::from_env());

    // MCP server
    let mcp_server = mcp::GdprServer::new(Arc::clone(&api_client));

    // HTTP anonymization proxy (:8080) — TODO T10: migrate to ApiClient
    let proxy_addr = std::env::var("PROXY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let vault_path = std::env::var("GDPR_VAULT_PATH")
        .unwrap_or_else(|_| "gdpr_vault.db".to_string());
    let model_dir = std::env::var("GLINER_MODEL_DIR")
        .unwrap_or_else(|_| "models/gliner-pii-edge".to_string());
    let pii_engine = gdpr_core::pii::engine::PiiEngine::load_production(&vault_path, &model_dir)?;
    let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(1, pii_engine)?);

    let session_cache = Arc::new(dashmap::DashMap::new());

    let proxy_state = Arc::new(proxy::ProxyState {
        engine_pool,
        http_client:  reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?,
        upstream_url: std::env::var("TENSORZERO_URL")
            .unwrap_or_else(|_| "http://localhost:3000/openai/v1".to_string()),
        upstream_key: std::env::var("TENSORZERO_KEY")
            .unwrap_or_else(|_| "hacienda".to_string()),
        session_cache: Arc::clone(&session_cache),
    });

    // Session GC: evict entries idle for > 30 minutes, every 5 minutes.
    let sc_gc = Arc::clone(&session_cache);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            sc_gc.retain(|_, v: &mut proxy::SessionCache| {
                v.created_at.elapsed() < std::time::Duration::from_secs(1800)
            });
        }
    });

    let app = Router::new()
        .route("/openai/v1/chat/completions", post(proxy::chat_completions))
        .with_state(proxy_state);
    let listener = tokio::net::TcpListener::bind(&proxy_addr).await?;
    tracing::info!(%proxy_addr, "anonymization proxy listening");

    // MCP stdio server
    tracing::info!("gdpr-mcp ready — listening on stdio");
    let running = mcp_server.serve(stdio()).await?;

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
