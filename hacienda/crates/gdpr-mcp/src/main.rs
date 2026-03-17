mod api_client;
mod mcp;
mod proxy;

use std::sync::Arc;

use axum::{routing::post, Router};
use rmcp::service::ServiceExt;
use rmcp::transport::io::stdio;

use api_client::ApiClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("gdpr-mcp starting (T5 thin client)");

    // T5: read gdpr-api coordinates — panic with clear message if missing
    let api_client = Arc::new(ApiClient::from_env());

    // MCP server
    let mcp_server = mcp::GdprServer::new(Arc::clone(&api_client));

    // HTTP anonymization proxy (:8080)
    let proxy_addr = std::env::var("PROXY_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let proxy_state = Arc::new(proxy::ProxyState {
        api_client:   Arc::clone(&api_client),
        http_client:  reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?,
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

    tracing::info!("gdpr-mcp ready — listening on stdio");
    let running = mcp_server.serve(stdio()).await?;

    tokio::select! {
        result = axum::serve(listener, app) => { result?; }
        result = running.waiting() => { result?; }
    }

    Ok(())
}
