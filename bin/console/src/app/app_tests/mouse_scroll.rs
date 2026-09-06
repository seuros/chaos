use super::*;
use crate::history_cell::PlainHistoryCell;
use crate::pager_overlay::PagerOverlay;
use crossterm::event::{MouseEvent, MouseEventKind};
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

fn wheel(kind: MouseEventKind) -> TuiEvent {
    TuiEvent::Mouse(MouseEvent {
        kind,
        column: 10,
        row: 10,
        modifiers: KeyModifiers::NONE,
    })
}

fn first_thread_row(app: &mut App) -> String {
    let Some(Overlay::Transcript(transcript)) = &mut app.overlay else {
        panic!("wheel should open thread history");
    };
    // Match the full inline overlay, not the small composer viewport.
    let area = Rect::new(0, 1, 100, 29);
    let mut buffer = Buffer::empty(area);
    transcript.render(area, &mut buffer);
    (0..area.width)
        .map(|x| buffer[(x, area.y + 1)].symbol())
        .collect::<String>()
        .trim()
        .to_string()
}

#[tokio::test]
async fn mouse_scroll_routes_into_thread_and_reaches_both_ends() {
    let mut app = make_test_app().await;
    let mut tui = make_test_tui();
    tui.terminal.set_viewport_area(Rect::new(0, 26, 100, 4));
    app.transcript_cells = vec![Arc::new(PlainHistoryCell::new(
        (0..60)
            .map(|row| Line::from(format!("thread {row}")))
            .collect(),
    ))];

    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollUp))
        .await
        .unwrap();
    // 24 visible content rows: bottom is 36, first wheel moves up 3.
    assert_eq!(first_thread_row(&mut app), "thread 33");

    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollDown))
        .await
        .unwrap();
    assert_eq!(first_thread_row(&mut app), "thread 36");
    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollDown))
        .await
        .unwrap();
    assert_eq!(first_thread_row(&mut app), "thread 36");

    for _ in 0..20 {
        app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollUp))
            .await
            .unwrap();
    }
    assert_eq!(first_thread_row(&mut app), "thread 0");

    app.handle_tui_event(
        &mut tui,
        TuiEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
    )
    .await
    .unwrap();
    assert!(app.overlay.is_none());
    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollUp))
        .await
        .unwrap();
    assert_eq!(first_thread_row(&mut app), "thread 33");
    app.close_transcript_overlay(&mut tui);
}

#[tokio::test]
async fn mouse_scroll_does_not_open_an_empty_thread_or_scroll_past_live_end() {
    let mut app = make_test_app().await;
    let mut tui = make_test_tui();
    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollUp))
        .await
        .unwrap();
    assert!(app.overlay.is_none());

    app.transcript_cells = vec![Arc::new(PlainHistoryCell::new(vec!["reply".into()]))];
    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollDown))
        .await
        .unwrap();
    assert!(app.overlay.is_none());
}
