//! Fire-and-forget metering middleware.
//!
//! Measures response latency and logs usage via `tracing::info!`.
//! Full ClickHouse write deferred to T8.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::http::Request;
use axum::response::Response;
use tower::{Layer, Service};

use crate::state::{AppState, AuthContext};

// ── Layer ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MeterLayer {
    state: AppState,
}

impl MeterLayer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for MeterLayer {
    type Service = MeterService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        MeterService {
            inner,
            _state: self.state.clone(),
        }
    }
}

// ── Service ──────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MeterService<S> {
    inner: S,
    _state: AppState,
}

impl<S, B> Service<Request<B>> for MeterService<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        // Capture tenant_id from extensions if present (set by AuthLayer)
        let tenant_id = req
            .extensions()
            .get::<AuthContext>()
            .map(|a| a.tenant_id.clone());
        let method = req.method().clone();
        let path = req.uri().path().to_string();

        Box::pin(async move {
            let start = Instant::now();
            let resp = inner.call(req).await?;
            let latency_ms = start.elapsed().as_millis();
            let status = resp.status().as_u16();

            // Fire-and-forget logging
            tokio::spawn(async move {
                tracing::info!(
                    tenant_id = tenant_id.as_deref().unwrap_or("anonymous"),
                    %method,
                    %path,
                    %status,
                    %latency_ms,
                    "request metered"
                );
            });

            Ok(resp)
        })
    }
}
