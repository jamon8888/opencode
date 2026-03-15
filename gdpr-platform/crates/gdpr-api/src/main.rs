use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod state;
mod error;
mod router;
mod handlers;
mod middleware;
mod clients;

pub use state::AppState;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    // I4 guard — refuse to start without vault key
    if std::env::var("CLOAKPIPE_VAULT_KEY")
        .map(|k| k.is_empty())
        .unwrap_or(true)
    {
        anyhow::bail!("CLOAKPIPE_VAULT_KEY must be set to a non-empty secret");
    }

    let db_path = std::env::var("GDPR_DB_PATH").unwrap_or_else(|_| "gdpr-api.db".to_string());
    let api_db_path = format!("{}.api.db", db_path.trim_end_matches(".db"));

    // Build API-specific deadpool (api_keys, sessions, usage)
    let api_cfg = deadpool_sqlite::Config::new(&api_db_path);
    let api_pool = api_cfg.create_pool(deadpool_sqlite::Runtime::Tokio1)?;
    {
        let conn = api_pool.get().await?;
        conn.interact(|c| {
            c.execute_batch(
                "
                PRAGMA busy_timeout=5000;
                PRAGMA journal_mode=WAL;
                CREATE TABLE IF NOT EXISTS api_keys (
                    id          TEXT PRIMARY KEY,
                    name        TEXT NOT NULL,
                    key_hash    TEXT NOT NULL,
                    created_at  INTEGER NOT NULL,
                    revoked     INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS usage_records (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    api_key_id  TEXT NOT NULL,
                    tokens_in   INTEGER NOT NULL DEFAULT 0,
                    tokens_out  INTEGER NOT NULL DEFAULT 0,
                    month       TEXT NOT NULL,
                    created_at  INTEGER NOT NULL
                );
            ",
            )?;
            Ok::<_, rusqlite::Error>(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("DB pool error: {e}"))?
        .map_err(|e| anyhow::anyhow!("DB init failed: {e}"))?;
    }
    {
        let conn2 = api_pool.get().await?;
        let _ = conn2.interact(|c| {
            // Idempotent: ignore error if column already exists
            let _ = c.execute("ALTER TABLE api_keys ADD COLUMN rotated_at INTEGER", []);
            Ok::<_, rusqlite::Error>(())
        }).await;
    }

    let upstream_url = std::env::var("TENSORZERO_URL")
        .unwrap_or_else(|_| "http://localhost:3000/openai/v1".to_string());

    let session_cache: Arc<dashmap::DashMap<String, state::SessionCache>> =
        Arc::new(dashmap::DashMap::new());
    let session_cache_gc = Arc::clone(&session_cache);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            interval.tick().await;
            session_cache_gc.retain(|_, v: &mut state::SessionCache| {
                v.created_at.elapsed() < Duration::from_secs(1800)
            });
        }
    });

    let vault_path = std::env::var("GDPR_VAULT_PATH")
        .unwrap_or_else(|_| "gdpr_vault.db".to_string());
    let model_dir = std::env::var("GLINER_MODEL_DIR")
        .unwrap_or_else(|_| "models/gliner-pii-edge".to_string());
    let pool_size: usize = std::env::var("ENGINE_POOL_SIZE")
        .ok().and_then(|s| s.parse().ok()).unwrap_or(4);
    let pii_engine = gdpr_core::pii::engine::PiiEngine::load_production(&vault_path, &model_dir)?;
    let engine_pool = Arc::new(gdpr_core::pii::pool::EnginePool::new(pool_size, pii_engine)?);

    // Optional ClickHouse audit client — skipped gracefully if CLICKHOUSE_URL not set
    let clickhouse = std::env::var("CLICKHOUSE_URL").ok().map(|url| {
        tracing::info!(%url, "ClickHouse audit trail enabled");
        crate::clients::ClickHouseClient::new(url, reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("ClickHouse reqwest client"))
    });

    let state = AppState {
        db: api_pool,
        keys: Arc::new(dashmap::DashMap::new()),
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?,
        upstream_url,
        session_cache,
        engine_pool,
        clickhouse,
    };

    let app  = router::build(state);
    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8443".to_string());

    let cert_path = std::env::var("TLS_CERT_PATH").ok();
    let key_path  = std::env::var("TLS_KEY_PATH").ok();

    match (cert_path, key_path) {
        (Some(cert), Some(key)) => {
            use axum_server::tls_rustls::RustlsConfig;

            let cert_bytes = tokio::fs::read(&cert).await?;
            let key_bytes  = tokio::fs::read(&key).await?;

            let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> =
                rustls_pemfile::certs(&mut cert_bytes.as_slice())
                    .collect::<Result<Vec<_>, _>>()?;
            if cert_chain.is_empty() {
                anyhow::bail!("TLS_CERT_PATH '{}': PEM file contains no certificate blocks", cert);
            }
            let private_key =
                rustls_pemfile::private_key(&mut key_bytes.as_slice())?
                    .ok_or_else(|| anyhow::anyhow!("TLS_KEY_PATH: no private key found"))?;

            let mut tls_cfg = rustls::ServerConfig::builder_with_protocol_versions(
                    &[&rustls::version::TLS13],
                )
                .with_no_client_auth()
                .with_single_cert(cert_chain, private_key)
                .map_err(|_| anyhow::anyhow!("TLS certificate/key pair is invalid — check TLS_CERT_PATH and TLS_KEY_PATH"))?;

            tls_cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

            let rustls_config = RustlsConfig::from_config(std::sync::Arc::new(tls_cfg));
            tracing::info!(%addr, "gdpr-api listening (TLS 1.3)");
            axum_server::bind_rustls(addr.parse()?, rustls_config)
                .serve(app.into_make_service())
                .await?;
        }
        _ => {
            tracing::warn!(%addr, "gdpr-api listening (plain HTTP — set TLS_CERT_PATH + TLS_KEY_PATH for production)");
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(listener, app).await?;
        }
    }
    Ok(())
}
