use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[tokio::test(start_paused = true)]
async fn opens_after_failure_and_rejects_without_running_operation() {
    let breaker = AsyncCircuitBreaker::new(
        "test",
        1,
        Duration::from_secs(60),
        Duration::from_secs(30),
        1,
    );
    let calls = AtomicUsize::new(0);

    let first = breaker
        .call(|| async {
            calls.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>("down")
        })
        .await;
    assert!(matches!(first, Err(BreakerError::Operation("down"))));

    let second = breaker
        .call(|| async {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<(), &str>(())
        })
        .await;
    assert!(matches!(second, Err(BreakerError::Open)));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn timeout_allows_a_probe_that_closes_the_breaker() {
    let breaker = AsyncCircuitBreaker::new(
        "test-recovery",
        1,
        Duration::from_secs(60),
        Duration::from_millis(10),
        1,
    );

    let first = breaker.call(|| async { Err::<(), _>("down") }).await;
    assert!(matches!(first, Err(BreakerError::Operation("down"))));
    assert_eq!(breaker.retry_after(), Some(Duration::from_millis(10)));

    tokio::time::advance(Duration::from_millis(9)).await;
    let rejected: Result<(), BreakerError<&str>> = breaker
        .call(|| async { panic!("probe must not run before the deadline") })
        .await;
    assert!(matches!(rejected, Err(BreakerError::Open)));
    assert_eq!(breaker.retry_after(), Some(Duration::from_millis(1)));

    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(breaker.retry_after(), Some(Duration::ZERO));
    let recovered = breaker.call(|| async { Ok::<_, &str>("up") }).await;

    assert!(matches!(recovered, Ok("up")));
    assert_eq!(breaker.retry_after(), None);
}
