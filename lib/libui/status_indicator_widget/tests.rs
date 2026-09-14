use super::*;
use crate::test_support::make_app_event_sender;
use crate::test_support::render_test_backend_debug;
macro_rules! assert_snapshot {
    ($($arg:tt)*) => {
        insta::with_settings!({ snapshot_path => "../snapshots" }, {
            insta::assert_snapshot!($($arg)*);
        })
    };
}
use std::time::Duration;
use std::time::Instant;

use pretty_assertions::assert_eq;

pub(crate) fn status_indicator_widget_suite() {
    fmt_elapsed_compact_formats_seconds_minutes_hours();
    renders_with_working_header();
    renders_truncated();
    renders_inline_progress_message();
    renders_wrapped_details_panama_two_lines();
    timer_pauses_when_requested();
    details_overflow_adds_ellipsis();
    details_args_can_disable_capitalization_and_limit_lines();
}
#[cfg(test)]
fn fmt_elapsed_compact_formats_seconds_minutes_hours() {
    assert_eq!(fmt_elapsed_compact(0), "0s");
    assert_eq!(fmt_elapsed_compact(1), "1s");
    assert_eq!(fmt_elapsed_compact(59), "59s");
    assert_eq!(fmt_elapsed_compact(60), "1m 00s");
    assert_eq!(fmt_elapsed_compact(61), "1m 01s");
    assert_eq!(fmt_elapsed_compact(3 * 60 + 5), "3m 05s");
    assert_eq!(fmt_elapsed_compact(59 * 60 + 59), "59m 59s");
    assert_eq!(fmt_elapsed_compact(3600), "1h 00m 00s");
    assert_eq!(fmt_elapsed_compact(3600 + 60 + 1), "1h 01m 01s");
    assert_eq!(fmt_elapsed_compact(25 * 3600 + 2 * 60 + 3), "25h 02m 03s");
}

#[cfg(test)]
fn renders_with_working_header() {
    let tx = make_app_event_sender();
    let w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

    assert_snapshot!(
        "renders_with_working_header",
        render_test_backend_debug(80, 2, |f| {
            w.render(f.area(), f.buffer_mut());
        })
    );
}

#[cfg(test)]
fn renders_truncated() {
    let tx = make_app_event_sender();
    let w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

    assert_snapshot!(
        "renders_truncated",
        render_test_backend_debug(20, 2, |f| {
            w.render(f.area(), f.buffer_mut());
        })
    );
}

#[cfg(test)]
fn renders_inline_progress_message() {
    let tx = make_app_event_sender();
    let mut w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), false);
    w.update_inline_message(Some("~1.2K tokens".to_string()));
    w.is_paused = true;
    w.elapsed_running = Duration::ZERO;

    let rendered = render_test_backend_debug(80, 1, |f| {
        w.render(f.area(), f.buffer_mut());
    });

    assert!(rendered.contains("~1.2K tokens"), "{rendered}");
}

#[cfg(test)]
fn renders_wrapped_details_panama_two_lines() {
    let tx = make_app_event_sender();
    let mut w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), false);
    w.update_details(
        Some("A man a plan a canal panama".to_string()),
        StatusDetailsCapitalization::CapitalizeFirst,
        STATUS_DETAILS_DEFAULT_MAX_LINES,
    );
    w.set_interrupt_hint_visible(false);

    // Freeze time-dependent rendering (elapsed + spinner) to keep the snapshot stable.
    w.is_paused = true;
    w.elapsed_running = Duration::ZERO;

    // Prefix is 4 columns, so a width of 30 yields a content width of 26: one column
    // short of fitting the whole phrase (27 cols), forcing exactly one wrap without ellipsis.
    assert_snapshot!(
        "renders_wrapped_details_panama_two_lines",
        render_test_backend_debug(30, 3, |f| {
            w.render(f.area(), f.buffer_mut());
        })
    );
}

#[cfg(test)]
fn timer_pauses_when_requested() {
    let tx = make_app_event_sender();
    let mut widget = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);

    let baseline = Instant::now();
    widget.last_resume_at = baseline;

    let before_pause = widget.elapsed_seconds_at(baseline + Duration::from_secs(5));
    assert_eq!(before_pause, 5);

    widget.pause_timer_at(baseline + Duration::from_secs(5));
    let paused_elapsed = widget.elapsed_seconds_at(baseline + Duration::from_secs(10));
    assert_eq!(paused_elapsed, before_pause);

    widget.resume_timer_at(baseline + Duration::from_secs(10));
    let after_resume = widget.elapsed_seconds_at(baseline + Duration::from_secs(13));
    assert_eq!(after_resume, before_pause + 3);
}

#[cfg(test)]
fn details_overflow_adds_ellipsis() {
    let tx = make_app_event_sender();
    let mut w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);
    w.update_details(
        Some("abcd abcd abcd abcd".to_string()),
        StatusDetailsCapitalization::CapitalizeFirst,
        STATUS_DETAILS_DEFAULT_MAX_LINES,
    );

    let lines = w.wrapped_details_lines(6);
    assert_eq!(lines.len(), STATUS_DETAILS_DEFAULT_MAX_LINES);
    let last = lines.last().expect("expected last details line");
    assert!(
        last.spans[1].content.as_ref().ends_with("…"),
        "expected ellipsis in last line: {last:?}"
    );
}

#[cfg(test)]
fn details_args_can_disable_capitalization_and_limit_lines() {
    let tx = make_app_event_sender();
    let mut w = StatusIndicatorWidget::new(tx, crate::tui::FrameRequester::test_dummy(), true);
    w.update_details(
        Some("cargo test -p chaos-kern and then cargo test -p chaos-console".to_string()),
        StatusDetailsCapitalization::Preserve,
        1,
    );

    assert_eq!(
        w.details(),
        Some("cargo test -p chaos-kern and then cargo test -p chaos-console")
    );

    let lines = w.wrapped_details_lines(24);
    assert_eq!(lines.len(), 1);
    let last = lines.last().expect("expected one details line");
    assert!(
        last.spans
            .last()
            .is_some_and(|span| span.content.as_ref().contains('…')),
        "expected one-line details to be ellipsized, got {last:?}"
    );
}
