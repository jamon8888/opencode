use std::time::Duration;

use gdpr_mcp::resilience::{Bulkhead, CircuitBreaker, UpstreamGuard};

#[tokio::test]
async fn test_circuit_breaker_opens_after_failures() {
    let cb = CircuitBreaker::new("test", 3, Duration::from_secs(60));
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open(), "circuit must be open after threshold failures");
}

#[tokio::test]
async fn test_circuit_breaker_rejects_when_open() {
    let cb = CircuitBreaker::new("test", 1, Duration::from_secs(60));
    cb.record_failure();
    let result = cb.call(async { Ok::<(), anyhow::Error>(()) }).await;
    assert!(result.is_err(), "open circuit must reject calls");
}

#[tokio::test]
async fn test_circuit_breaker_resets_on_success() {
    let cb = CircuitBreaker::new("test", 3, Duration::from_millis(50));
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open());
    tokio::time::sleep(Duration::from_millis(60)).await;
    // HalfOpen: next successful call should close it
    let result = cb.call(async { Ok::<(), anyhow::Error>(()) }).await;
    assert!(result.is_ok(), "half-open call should succeed");
    assert!(!cb.is_open(), "circuit must close after successful half-open probe");
}

#[tokio::test]
async fn test_bulkhead_limits_concurrency() {
    let bh = Bulkhead::new("test", 2);
    let _g1 = bh.try_acquire().unwrap();
    let _g2 = bh.try_acquire().unwrap();
    let g3 = bh.try_acquire();
    assert!(g3.is_err(), "bulkhead at capacity must reject");
}

#[tokio::test]
async fn test_upstream_guard_propagates_result() {
    let guard = UpstreamGuard::new("test", 5, Duration::from_secs(30), 10);
    let result = guard.call(async { Ok::<i32, anyhow::Error>(42) }).await;
    assert_eq!(result.unwrap(), 42);
}
