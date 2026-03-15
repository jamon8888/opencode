use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod state;
mod error;
mod router;
mod handlers;
mod middleware;

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
        .ok();
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
                v.last_used.elapsed() < Duration::from_secs(1800)
            });
        }
    });

    let state = AppState {
        db: api_pool,
        keys: Arc::new(dashmap::DashMap::new()),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?,
        upstream_url,
        session_cache,
    };

    let app = router::build(state);
    let addr =
        std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3001".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("gdpr-api listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
