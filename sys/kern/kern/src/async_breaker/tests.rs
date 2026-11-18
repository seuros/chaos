use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[tokio::test(start_paused = true)]
async fn opens_after_failure_and_rejects_without_running_operation() {
    const POLICY: Policy = breaker_policy(1, Duration::from_secs(60), Duration::from_secs(30), 1);
    let breaker = AsyncCircuitBreaker::new("test", POLICY);
    let calls = AtomicUsize::new(0);

    let first = breaker
        .call(|| async {
            calls.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>("down")
        })
        .await;
    std::assert_matches!(first, Err(BreakerError::Operation("down")));

    let second = breaker
        .call(|| async {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<(), &str>(())
        })
        .await;
    std::assert_matches!(second, Err(BreakerError::Open));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}
