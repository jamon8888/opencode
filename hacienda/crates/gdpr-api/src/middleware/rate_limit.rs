//! Per-tenant rate limiting middleware using governor.
//!
//! Extracts AuthContext from request extensions, applies a per-tenant quota
//! derived from `plan.rate_limit_rpm()`, and rejects with `ApiError::RateLimit`
//! (429 + Retry-After with ±5s jitter) when the bucket is exhausted.
//!
//! If no AuthContext is present (public route before auth), requests pass through.

use std::{
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use governor::{
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::keyed::DefaultKeyedStateStore,
    Quota, RateLimiter,
};
use rand::Rng;
use tower::{Layer, Service};

use crate::error::ApiError;
use crate::state::AuthContext;

type KeyedLimiter = Arc<
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock, NoOpMiddleware>,
>;

/// Per-plan limiters, lazily created and stored by RPM quota.
/// Keyed by (rpm) so all tenants on the same plan share a limiter type,
/// but governor's keyed state ensures per-tenant bucket isolation.
#[derive(Clone)]
struct LimiterStore {
    limiters: Arc<DashMap<u32, KeyedLimiter>>,
}

impl LimiterStore {
    fn new() -> Self {
        Self { limiters: Arc::new(DashMap::new()) }
    }

    fn get_or_create(&self, rpm: u32) -> KeyedLimiter {
        self.limiters
            .entry(rpm)
            .or_insert_with(|| {
                let quota = Quota::per_minute(
                    NonZeroU32::new(rpm.max(1)).expect("rpm > 0"),
                );
                Arc::new(RateLimiter::keyed(quota))
            })
            .clone()
    }
}

// ── Layer ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RateLimitLayer {
    store: LimiterStore,
}

impl RateLimitLayer {
    pub fn new() -> Self {
        Self { store: LimiterStore::new() }
    }
}

impl Default for RateLimitLayer {
    fn default() -> Self { Self::new() }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimitService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RateLimitService { inner, store: self.store.clone() }
    }
}

// ── Service ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RateLimitService<S> {
    inner: S,
    store: LimiterStore,
}

impl<S, B> Service<Request<B>> for RateLimitService<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error    = S::Error;
    type Future   = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        // Extract AuthContext if available (may be absent on public routes)
        let auth_ctx = req.extensions().get::<AuthContext>().cloned();
        let store = self.store.clone();
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            if let Some(ref ctx) = auth_ctx {
                let rpm = ctx.plan.rate_limit_rpm();
                let limiter = store.get_or_create(rpm);
                if limiter.check_key(&ctx.tenant_id).is_err() {
                    // Jitter: 60 ± 5 seconds
                    let jitter: u64 = rand::thread_rng().gen_range(0..=10);
                    let retry_after = 55 + jitter; // [55, 65]
                    return Ok(ApiError::RateLimit { retry_after_secs: retry_after }.into_response());
                }
            }
            inner.call(req).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Plan;

    #[test]
    fn test_limiter_store_creates_entry() {
        let store = LimiterStore::new();
        let l1 = store.get_or_create(60);
        let l2 = store.get_or_create(60);
        // Same RPM → same Arc pointer
        assert!(Arc::ptr_eq(&l1, &l2));
    }

    #[test]
    fn test_different_rpm_different_limiter() {
        let store = LimiterStore::new();
        let l1 = store.get_or_create(60);
        let l2 = store.get_or_create(300);
        assert!(!Arc::ptr_eq(&l1, &l2));
    }

    #[test]
    fn test_plan_rpm_values() {
        assert_eq!(Plan::Starter.rate_limit_rpm(), 60);
        assert_eq!(Plan::Business.rate_limit_rpm(), 300);
        assert_eq!(Plan::Enterprise.rate_limit_rpm(), 1000);
    }
}
