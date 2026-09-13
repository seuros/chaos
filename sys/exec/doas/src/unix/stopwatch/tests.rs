use super::Stopwatch;
use tokio::time::Duration;
use tokio::time::sleep;
use tokio::time::timeout;

#[tokio::test]
async fn cancellation_receiver_fires_after_limit() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();

    assert!(
        timeout(Duration::from_millis(30), token.cancelled())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(250), token.cancelled())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn pause_prevents_timeout_until_resumed() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();

    let pause_handle = tokio::spawn({
        let stopwatch = stopwatch.clone();
        async move {
            stopwatch
                .pause_for(async {
                    sleep(Duration::from_millis(100)).await;
                })
                .await;
        }
    });

    assert!(
        timeout(Duration::from_millis(30), token.cancelled())
            .await
            .is_err()
    );

    pause_handle.await.expect("pause task should finish");

    token.cancelled().await;
}

#[tokio::test]
async fn overlapping_pauses_only_resume_once() {
    let stopwatch = Stopwatch::new(Duration::from_millis(50));
    let token = stopwatch.cancellation_token();

    // First pause.
    let pause1 = {
        let stopwatch = stopwatch.clone();
        tokio::spawn(async move {
            stopwatch
                .pause_for(async {
                    sleep(Duration::from_millis(80)).await;
                })
                .await;
        })
    };

    // Overlapping pause that ends sooner.
    let pause2 = {
        let stopwatch = stopwatch.clone();
        tokio::spawn(async move {
            stopwatch
                .pause_for(async {
                    sleep(Duration::from_millis(30)).await;
                })
                .await;
        })
    };

    // While both pauses are active, the cancellation should not fire.
    assert!(
        timeout(Duration::from_millis(40), token.cancelled())
            .await
            .is_err()
    );

    pause2.await.expect("short pause should complete");

    // Still paused because the long pause is active.
    assert!(
        timeout(Duration::from_millis(30), token.cancelled())
            .await
            .is_err()
    );

    pause1.await.expect("long pause should complete");

    // Now the stopwatch should resume and hit the limit shortly after.
    token.cancelled().await;
}

#[tokio::test]
async fn unlimited_stopwatch_never_cancels() {
    let stopwatch = Stopwatch::unlimited();
    let token = stopwatch.cancellation_token();

    assert!(
        timeout(Duration::from_millis(30), token.cancelled())
            .await
            .is_err()
    );
}
