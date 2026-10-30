use super::*;
use crate::client_common::ResponseEvent;
use tokio::sync::{mpsc, oneshot};

#[tokio::test]
async fn terminal_event_waits_for_worker_exit_and_overlap_is_rejected() {
    let client = test_model_client(SessionSource::Exec);
    let (output, mut response) = mpsc::channel(8);
    let (events, input) = mpsc::channel(8);
    let (ready, reached) = oneshot::channel();
    let (release, finish) = oneshot::channel();
    client
        .spawn_clamp_worker(output, input, async move {
            events.send(Ok(ResponseEvent::Created)).await.unwrap();
            events
                .send(Ok(ResponseEvent::Completed {
                    response_id: "native".into(),
                    token_usage: None,
                }))
                .await
                .unwrap();
            ready.send(()).unwrap();
            finish.await.unwrap();
        })
        .await
        .unwrap();
    reached.await.unwrap();
    assert!(client.clamp_is_pending());
    std::assert_matches!(response.recv().await, Some(Ok(ResponseEvent::Created)));
    assert!(
        response.try_recv().is_err(),
        "terminal result cannot pass an active worker"
    );
    let (output, _unused) = mpsc::channel(1);
    let (_events, input) = mpsc::channel(1);
    assert!(
        client
            .spawn_clamp_worker(output, input, async {})
            .await
            .is_err()
    );
    release.send(()).unwrap();
    std::assert_matches!(
        response.recv().await,
        Some(Ok(ResponseEvent::Completed { .. }))
    );
    assert!(!client.clamp_is_pending());
    client.stop_clamp_worker().await.unwrap();
}

#[tokio::test]
async fn cancellation_drops_the_driving_future_before_clearing_pending() {
    struct Dropped(Option<oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let client = test_model_client(SessionSource::Exec);
    let (output, response) = mpsc::channel(1);
    let (events, input) = mpsc::channel(1);
    let (ready, reached) = oneshot::channel();
    let (dropped, observed) = oneshot::channel();
    client
        .spawn_clamp_worker(output, input, async move {
            let _guard = Dropped(Some(dropped));
            let _events = events;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        })
        .await
        .unwrap();
    reached.await.unwrap();
    assert!(client.clamp_is_pending());
    drop(response);
    client.stop_clamp_worker().await.unwrap();
    observed.await.unwrap();
    assert!(!client.clamp_is_pending());
    client.stop_clamp_worker().await.unwrap();
}

#[tokio::test]
async fn panic_and_missing_result_are_non_retryable_native_failures() {
    for panic in [false, true] {
        let client = test_model_client(SessionSource::Exec);
        let (output, mut response) = mpsc::channel(1);
        let (events, input) = mpsc::channel(1);
        client
            .spawn_clamp_worker(output, input, async move {
                drop(events);
                assert!(!panic, "synthetic native worker panic");
            })
            .await
            .unwrap();
        std::assert_matches!(
            response.recv().await,
            Some(Err(chaos_parrot::error::ApiError::InvalidRequest { .. }))
        );
        assert!(!client.clamp_is_pending());
        client.stop_clamp_worker().await.unwrap();
    }
}
