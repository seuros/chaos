use std::future::Future;

fn run_async(future: impl Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tui test runtime")
        .block_on(future);
}

pub(crate) fn tui_suite() {
    run_async(super::event_stream::tests::event_stream_suite());
    super::frame_rate_limiter::tests::frame_rate_limiter_suite();
    super::frame_requester::tests::frame_requester_suite();
}

#[cfg(feature = "vt100-tests")]
#[tokio::test]
async fn test_constructor_uses_fixed_capabilities() {
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let terminal = super::Terminal::new_for_test(backend, 100, 30);
    let mut tui = super::Tui::new_for_test(terminal);

    assert!(!tui.enhanced_keys_supported);
    assert!(tui.notification_backend.is_none());
    assert_eq!(
        tui.terminal.last_known_screen_size,
        ratatui::layout::Size::new(100, 30)
    );

    let mut resumed = tui.event_broker.resume_events_rx();
    let result = tui
        .with_restored(super::RestoreMode::KeepRaw, || async {
            Err::<(), _>("editor failed")
        })
        .await;
    assert_eq!(result, Err("editor failed"));
    assert!(resumed.has_changed().unwrap());
    resumed.borrow_and_update();

    let mut operation =
        Box::pin(tui.with_restored(super::RestoreMode::KeepRaw, std::future::pending::<()>));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(operation.as_mut().poll(&mut cx).is_pending());
    assert!(!resumed.has_changed().unwrap());
    drop(operation);
    assert!(
        resumed.has_changed().unwrap(),
        "cancellation must resume input"
    );
}

#[test]
fn bottom_anchored_viewport_tracks_resize_without_repositioning_other_views() {
    use ratatui::layout::{Rect, Size};
    for (viewport, terminal, expected) in [
        (
            Rect::new(0, 18, 80, 6),
            Size::new(120, 40),
            Some(Rect::new(0, 34, 80, 6)),
        ),
        (
            Rect::new(0, 18, 80, 6),
            Size::new(80, 12),
            Some(Rect::new(0, 6, 80, 6)),
        ),
        (Rect::new(0, 10, 80, 6), Size::new(120, 40), None),
    ] {
        assert_eq!(
            super::bottom_anchored_viewport_after_resize(viewport, Size::new(80, 24), terminal, 0),
            expected,
            "viewport {viewport:?}, terminal {terminal:?}"
        );
    }
}

#[test]
fn terminal_title_sanitizer_strips_controls_collapses_space_and_truncates() {
    assert_eq!(
        super::sanitize_terminal_title("  Compaction\n\x1b]0;bad\x07  timing  "),
        "Compaction ]0;bad timing"
    );
    assert_eq!(
        super::sanitize_terminal_title(&"a".repeat(super::MAX_TERMINAL_TITLE_WIDTH + 5)),
        "a".repeat(super::MAX_TERMINAL_TITLE_WIDTH)
    );
}
