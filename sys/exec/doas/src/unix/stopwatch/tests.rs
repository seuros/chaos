use super::Stopwatch;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio::time::advance;
use tokio::time::timeout;
use tokio_test::assert_pending;
use tokio_test::assert_ready;

#[tokio::test(start_paused = true)]
async fn cancellation_receiver_fires_after_limit() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();
    let started = Instant::now();

    advance(Duration::from_millis(49)).await;
    assert!(!token.is_cancelled());
    timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("stopwatch should cancel at its limit");
    assert_eq!(started.elapsed(), Duration::from_millis(50));
}

#[tokio::test(start_paused = true)]
async fn pause_prevents_timeout_until_resumed() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();

    // Consume part of the budget before pausing. Resuming must preserve it.
    advance(Duration::from_millis(20)).await;
    let (resume, paused) = oneshot::channel();
    let mut pause = tokio_test::task::spawn(stopwatch.pause_for(paused));
    assert_pending!(pause.poll());

    advance(Duration::from_secs(60)).await;
    assert!(!token.is_cancelled());

    resume.send(47).expect("release pause");
    assert_eq!(assert_ready!(pause.poll()), Ok(47));
    let resumed_at = Instant::now();
    advance(Duration::from_millis(29)).await;
    assert!(!token.is_cancelled());
    timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("stopwatch should resume");
    assert_eq!(resumed_at.elapsed(), Duration::from_millis(30));
}

#[tokio::test(start_paused = true)]
async fn overlapping_pauses_only_resume_once() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();

    let (resume1, paused1) = oneshot::channel();
    let (resume2, paused2) = oneshot::channel();
    let mut pause1 = tokio_test::task::spawn(stopwatch.pause_for(paused1));
    let mut pause2 = tokio_test::task::spawn(stopwatch.pause_for(paused2));
    assert_pending!(pause1.poll());
    assert_pending!(pause2.poll());

    advance(Duration::from_secs(60)).await;
    assert!(!token.is_cancelled());

    resume2.send(()).expect("release second pause");
    assert_ready!(pause2.poll()).expect("second pause completes");
    advance(Duration::from_secs(60)).await;
    assert!(!token.is_cancelled(), "first pause still holds the clock");

    resume1.send(()).expect("release first pause");
    assert_ready!(pause1.poll()).expect("first pause completes");
    let resumed_at = Instant::now();
    timeout(Duration::from_secs(1), token.cancelled())
        .await
        .expect("last pause should resume the clock");
    assert_eq!(resumed_at.elapsed(), Duration::from_millis(50));
}

#[tokio::test(start_paused = true)]
async fn unlimited_stopwatch_never_cancels() {
    let stopwatch = Stopwatch::unlimited();
    let token = stopwatch.cancellation_token();

    advance(Duration::from_secs(60 * 60 * 24 * 365)).await;
    assert!(!token.is_cancelled());
}
