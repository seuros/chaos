use super::super::frame_rate_limiter::MIN_FRAME_INTERVAL;
use super::*;
use tokio::time;
use tokio_util::time::FutureExt;

pub(crate) fn frame_requester_suite() {
    run_paused(test_schedule_frame_immediate_triggers_once());
    run_paused(test_schedule_frame_in_triggers_at_delay());
    run_paused(test_coalesces_multiple_requests_into_single_draw());
    run_paused(test_coalesces_mixed_immediate_and_delayed_requests());
    run_paused(test_limits_draw_notifications_to_120fps());
    run_paused(test_rate_limit_clamps_early_delayed_requests());
    run_paused(test_rate_limit_does_not_delay_future_draws());
    run_paused(test_multiple_delayed_requests_coalesce_to_earliest());
}

fn run_paused(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .expect("build paused tokio runtime")
        .block_on(future);
}

async fn test_schedule_frame_immediate_triggers_once() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    requester.schedule_frame();

    // Advance time minimally to let the scheduler process and hit the deadline == now.
    time::advance(Duration::from_millis(1)).await;

    // First draw should arrive.
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for first draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    // No second draw should arrive.
    let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
    assert!(second.is_err(), "unexpected extra draw received");
}

async fn test_schedule_frame_in_triggers_at_delay() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    requester.schedule_frame_in(Duration::from_millis(50));

    // Advance less than the delay: no draw yet.
    time::advance(Duration::from_millis(30)).await;
    let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
    assert!(early.is_err(), "draw fired too early");

    // Advance past the deadline: one draw should fire.
    time::advance(Duration::from_millis(25)).await;
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for scheduled draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    // No second draw should arrive.
    let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
    assert!(second.is_err(), "unexpected extra draw received");
}

async fn test_coalesces_multiple_requests_into_single_draw() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    // Schedule multiple immediate requests close together.
    requester.schedule_frame();
    requester.schedule_frame();
    requester.schedule_frame();

    // Allow the scheduler to process and hit the coalesced deadline.
    time::advance(Duration::from_millis(1)).await;

    // Expect only a single draw notification despite three requests.
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for coalesced draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    // No additional draw should be sent for the same coalesced batch.
    let second = draw_rx.recv().timeout(Duration::from_millis(20)).await;
    assert!(second.is_err(), "unexpected extra draw received");
}

async fn test_coalesces_mixed_immediate_and_delayed_requests() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    // Schedule a delayed draw and then an immediate one; should coalesce and fire at the earliest (immediate).
    requester.schedule_frame_in(Duration::from_millis(100));
    requester.schedule_frame();

    time::advance(Duration::from_millis(1)).await;

    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for coalesced immediate draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    // The later delayed request should have been coalesced into the earlier one; no second draw.
    let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
    assert!(second.is_err(), "unexpected extra draw received");
}

async fn test_limits_draw_notifications_to_120fps() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    requester.schedule_frame();
    time::advance(Duration::from_millis(1)).await;
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for first draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    requester.schedule_frame();
    time::advance(Duration::from_millis(1)).await;
    let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
    assert!(
        early.is_err(),
        "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
    );

    time::advance(MIN_FRAME_INTERVAL).await;
    let second = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for second draw");
    assert!(second.is_ok(), "broadcast closed unexpectedly");
}

async fn test_rate_limit_clamps_early_delayed_requests() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    requester.schedule_frame();
    time::advance(Duration::from_millis(1)).await;
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for first draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    requester.schedule_frame_in(Duration::from_millis(1));

    time::advance(MIN_FRAME_INTERVAL / 2).await;
    let too_early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
    assert!(
        too_early.is_err(),
        "draw fired too early; expected max 120fps (min interval {MIN_FRAME_INTERVAL:?})"
    );

    time::advance(MIN_FRAME_INTERVAL).await;
    let second = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for clamped draw");
    assert!(second.is_ok(), "broadcast closed unexpectedly");
}

async fn test_rate_limit_does_not_delay_future_draws() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    requester.schedule_frame();
    time::advance(Duration::from_millis(1)).await;
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for first draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    requester.schedule_frame_in(Duration::from_millis(50));

    time::advance(Duration::from_millis(49)).await;
    let early = draw_rx.recv().timeout(Duration::from_millis(1)).await;
    assert!(early.is_err(), "draw fired too early");

    time::advance(Duration::from_millis(1)).await;
    let second = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for delayed draw");
    assert!(second.is_ok(), "broadcast closed unexpectedly");
}

async fn test_multiple_delayed_requests_coalesce_to_earliest() {
    let (draw_tx, mut draw_rx) = broadcast::channel(16);
    let requester = FrameRequester::new(draw_tx);

    // Schedule multiple delayed draws; they should coalesce to the earliest (10ms).
    requester.schedule_frame_in(Duration::from_millis(100));
    requester.schedule_frame_in(Duration::from_millis(20));
    requester.schedule_frame_in(Duration::from_millis(120));

    // Advance to just before the earliest deadline: no draw yet.
    time::advance(Duration::from_millis(10)).await;
    let early = draw_rx.recv().timeout(Duration::from_millis(10)).await;
    assert!(early.is_err(), "draw fired too early");

    // Advance past the earliest deadline: one draw should fire.
    time::advance(Duration::from_millis(20)).await;
    let first = draw_rx
        .recv()
        .timeout(Duration::from_millis(50))
        .await
        .expect("timed out waiting for earliest coalesced draw");
    assert!(first.is_ok(), "broadcast closed unexpectedly");

    // No additional draw should fire for the later delayed requests.
    let second = draw_rx.recv().timeout(Duration::from_millis(120)).await;
    assert!(second.is_err(), "unexpected extra draw received");
}
