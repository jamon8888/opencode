use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::error::GdprError;

const CLOSED: u8 = 0;
const OPEN: u8 = 1;
const HALF_OPEN: u8 = 2;

// ── CircuitBreaker ────────────────────────────────────────────────────────────

pub struct CircuitBreaker {
    service: &'static str,
    state: AtomicU8,
    failure_threshold: u32,
    cooldown: Duration,
    inner: Mutex<BreakerInner>,
}

struct BreakerInner {
    failures: u32,
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    pub fn new(service: &'static str, failure_threshold: u32, cooldown: Duration) -> Arc<Self> {
        Arc::new(Self {
            service,
            state: AtomicU8::new(CLOSED),
            failure_threshold,
            cooldown,
            inner: Mutex::new(BreakerInner { failures: 0, opened_at: None }),
        })
    }

    pub fn is_open(&self) -> bool {
        match self.state.load(Ordering::Acquire) {
            OPEN => {
                let inner = self.inner.lock();
                if let Some(opened_at) = inner.opened_at {
                    if opened_at.elapsed() >= self.cooldown {
                        drop(inner);
                        self.state.store(HALF_OPEN, Ordering::Release);
                        false
                    } else {
                        true
                    }
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    pub fn record_failure(&self) {
        let mut inner = self.inner.lock();
        inner.failures += 1;
        if inner.failures >= self.failure_threshold {
            inner.opened_at = Some(Instant::now());
            self.state.store(OPEN, Ordering::Release);
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.inner.lock();
        inner.failures = 0;
        inner.opened_at = None;
        self.state.store(CLOSED, Ordering::Release);
    }

    pub async fn call<F, T, E>(&self, f: F) -> Result<T, GdprError>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        if self.is_open() {
            return Err(GdprError::CircuitOpen { service: self.service });
        }
        match f.await {
            Ok(v) => { self.record_success(); Ok(v) }
            Err(e) => { self.record_failure(); Err(GdprError::Extraction(e.to_string())) }
        }
    }
}

// ── Bulkhead ──────────────────────────────────────────────────────────────────

pub struct Bulkhead {
    name: &'static str,
    semaphore: tokio::sync::Semaphore,
}

impl Bulkhead {
    pub fn new(name: &'static str, max_concurrent: usize) -> Arc<Self> {
        Arc::new(Self { name, semaphore: tokio::sync::Semaphore::new(max_concurrent) })
    }

    pub fn try_acquire(&self) -> anyhow::Result<tokio::sync::SemaphorePermit<'_>> {
        self.semaphore.try_acquire()
            .map_err(|_| anyhow::anyhow!("bulkhead full: {}", self.name))
    }
}

// ── UpstreamGuard ─────────────────────────────────────────────────────────────

pub struct UpstreamGuard {
    cb: Arc<CircuitBreaker>,
    bh: Arc<Bulkhead>,
    timeout_secs: u64,
}

impl UpstreamGuard {
    pub fn new(
        name: &'static str,
        cb_threshold: u32,
        cb_recovery: Duration,
        bh_max: usize,
    ) -> Self {
        Self {
            cb: CircuitBreaker::new(name, cb_threshold, cb_recovery),
            bh: Bulkhead::new(name, bh_max),
            timeout_secs: 30,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub async fn call<F, T, E>(&self, fut: F) -> anyhow::Result<T>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let _permit = self.bh.try_acquire()?;
        if self.cb.is_open() {
            return Err(anyhow::anyhow!("circuit open: {}", self.cb.service));
        }
        match tokio::time::timeout(Duration::from_secs(self.timeout_secs), fut).await {
            Ok(Ok(v)) => { self.cb.record_success(); Ok(v) }
            Ok(Err(e)) => { self.cb.record_failure(); Err(anyhow::anyhow!("{e}")) }
            Err(_) => { self.cb.record_failure(); Err(anyhow::anyhow!("upstream timeout")) }
        }
    }
}
