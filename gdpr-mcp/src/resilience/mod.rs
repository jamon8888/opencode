use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::error::GdprError;

const CLOSED: u8 = 0;
const OPEN: u8 = 1;
const HALF_OPEN: u8 = 2;

/// Simple circuit breaker protecting a named upstream service.
///
/// State machine: Closed → Open (on failure) → HalfOpen (after cooldown) → Closed (on success).
pub struct CircuitBreaker {
    service: &'static str,
    state: AtomicU8,
    failure_threshold: u32,
    cooldown: Duration,
    /// Failure count and time the breaker opened, protected by a Mutex.
    inner: Mutex<BreakerInner>,
}

struct BreakerInner {
    failures: u32,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    pub fn new(service: &'static str) -> Arc<Self> {
        Arc::new(Self {
            service,
            state: AtomicU8::new(CLOSED),
            failure_threshold: 5,
            cooldown: Duration::from_secs(30),
            inner: Mutex::new(BreakerInner { failures: 0, opened_at: None }),
        })
    }

    /// Call `f` if the breaker is closed or half-open.
    /// On success: reset failures, transition to Closed.
    /// On failure: increment counter, trip to Open after threshold.
    pub async fn call<F, T, E>(&self, f: F) -> Result<T, GdprError>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        match self.state.load(Ordering::Acquire) {
            OPEN => {
                // Check if cooldown elapsed → try HalfOpen
                let inner = self.inner.lock().await;
                if let Some(opened_at) = inner.opened_at {
                    if opened_at.elapsed() >= self.cooldown {
                        drop(inner);
                        self.state.store(HALF_OPEN, Ordering::Release);
                    } else {
                        return Err(GdprError::CircuitOpen { service: self.service });
                    }
                } else {
                    return Err(GdprError::CircuitOpen { service: self.service });
                }
            }
            _ => {} // CLOSED or HALF_OPEN: proceed
        }

        match f.await {
            Ok(v) => {
                self.on_success().await;
                Ok(v)
            }
            Err(e) => {
                self.on_failure().await;
                Err(GdprError::Extraction(e.to_string()))
            }
        }
    }

    async fn on_success(&self) {
        let mut inner = self.inner.lock().await;
        inner.failures = 0;
        inner.opened_at = None;
        self.state.store(CLOSED, Ordering::Release);
    }

    async fn on_failure(&self) {
        let mut inner = self.inner.lock().await;
        inner.failures += 1;
        if inner.failures >= self.failure_threshold {
            inner.opened_at = Some(Instant::now());
            self.state.store(OPEN, Ordering::Release);
            tracing::warn!(service = self.service, "circuit breaker tripped OPEN");
        }
    }
}
