//! Rate-limit jitter middleware.
//!
//! Intercepts 429 responses and rewrites the `Retry-After` header with a
//! ±5-second random jitter to prevent thundering-herd re-connection spikes.
//!
//! The base retry interval is read from the inner `Retry-After` header if
//! present (parsed as seconds), or defaults to 60 seconds. The jittered value
//! is clamped to [1, ∞) seconds.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use axum::http::{HeaderValue, Request, Response, StatusCode};
use rand::Rng;
use tower::{Layer, Service};

// ── Layer ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct RateLimitJitterLayer;

impl<S> Layer<S> for RateLimitJitterLayer {
    type Service = RateLimitJitter<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitJitter { inner }
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RateLimitJitter<S> {
    inner: S,
}

impl<S, ReqBody, ResBody> Service<Request<ReqBody>> for RateLimitJitter<S>
where
    S: Service<Request<ReqBody>, Response = Response<ResBody>> + Send + Clone + 'static,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Send + 'static,
{
    type Response = Response<ResBody>;
    type Error    = S::Error;
    type Future   = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let mut inner = self.inner.clone();
        // Poll inner once to keep it ready (clone-swap pattern for Tower services)
        std::mem::swap(&mut self.inner, &mut inner);
        Box::pin(async move {
            let mut resp = inner.call(req).await?;
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                // Parse existing Retry-After value (seconds) or default to 60
                let base: i64 = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(60);
                // ±5 s jitter, clamped to minimum 1 second
                let jitter: i64 = rand::thread_rng().gen_range(-5..=5);
                let jittered = (base + jitter).max(1);
                if let Ok(val) = HeaderValue::from_str(&jittered.to_string()) {
                    resp.headers_mut().insert("retry-after", val);
                }
            }
            Ok(resp)
        })
    }
}
