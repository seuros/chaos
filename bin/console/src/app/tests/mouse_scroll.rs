use super::*;
use crate::history_cell::PlainHistoryCell;
use crate::pager_overlay::PagerOverlay;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
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
    let (mut app, _events, mut ops) = make_test_app_with_channels().await;
    let mut tui = make_test_tui();
    tui.terminal.set_viewport_area(Rect::new(0, 26, 100, 4));
    app.transcript_cells = vec![Arc::new(PlainHistoryCell::new(
        (0..60)
            .map(|row| Line::from(format!("thread {row}")))
            .collect(),
    ))];

    let palette = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT);
    let f2 = KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE);
    app.chat_widget
        .set_composer_text("draft".into(), Vec::new(), Vec::new());
    let draft = app.chat_widget.composer_text_with_pending();
    for (open, finish) in [
        (f2, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        (
            palette,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ),
        (palette, palette),
        (f2, f2),
    ] {
        app.handle_key_event(&mut tui, open).await;
        app.handle_key_event(
            &mut tui,
            KeyEvent {
                kind: KeyEventKind::Repeat,
                ..open
            },
        )
        .await;
        assert!(app.tile_manager.runtime.is_palette_open());
        for event in [
            wheel(MouseEventKind::ScrollUp),
            TuiEvent::Paste("private clipboard".into()),
            TuiEvent::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        ] {
            app.handle_tui_event(&mut tui, event).await.unwrap();
        }
        assert!(app.overlay.is_none());
        app.handle_key_event(&mut tui, finish).await;
        assert!(!app.tile_manager.runtime.is_palette_open());
        assert!(app.tile_manager.is_single_pane());
        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    }
    while ops.try_recv().is_ok() {}
    let mut tools = None;
    for open in [palette, f2] {
        app.handle_key_event(&mut tui, open).await;
        app.handle_key_event(
            &mut tui,
            KeyEvent::new_with_kind(
                KeyCode::Char('T'),
                KeyModifiers::SHIFT,
                KeyEventKind::Repeat,
            ),
        )
        .await;
        app.handle_key_event(&mut tui, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;
        let id = app.tile_manager.find_pane(PaneKind::ToolList).unwrap();
        assert_eq!(app.tile_manager.focused(), Some(id));
        if let Some(previous) = tools {
            assert_eq!(id, previous);
            assert!(ops.try_recv().is_err(), "focusing must not reload tools");
        } else {
            assert_matches!(ops.try_recv().unwrap(), Op::ListAllTools);
            tools = Some(id);
        }
        app.on_all_tools_received(
            &mut tui,
            chaos_ipc::protocol::AllToolsResponseEvent { tools: Vec::new() },
        );
        assert_eq!(app.tile_manager.find_pane(PaneKind::ToolList), Some(id));
    }
    app.handle_key_event(&mut tui, palette).await;
    app.handle_key_event(&mut tui, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .await;
    app.handle_key_event(
        &mut tui,
        KeyEvent::new_with_kind(KeyCode::Esc, KeyModifiers::NONE, KeyEventKind::Repeat),
    )
    .await;
    assert_eq!(app.tile_manager.find_pane(PaneKind::ToolList), tools);
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    app.handle_event(&mut tui, AppEvent::ToggleToolList)
        .await
        .unwrap();
    app.on_all_tools_received(
        &mut tui,
        chaos_ipc::protocol::AllToolsResponseEvent { tools: Vec::new() },
    );
    assert!(
        app.tile_manager.is_single_pane(),
        "late replies must not reopen tools"
    );
    app.handle_key_event(&mut tui, palette).await;
    app.handle_tui_event(&mut tui, wheel(MouseEventKind::ScrollUp))
        .await
        .unwrap();
    assert!(app.overlay.is_none());
    app.open_transcript_overlay(&mut tui, None);
    assert!(!app.tile_manager.runtime.is_palette_open());
    app.close_transcript_overlay(&mut tui);
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

    // The same events in the Inspector belong to its plugin, not the transcript
    // or composer. Reuse this routing fixture rather than a separate UI suite.
    let area = Rect::new(0, 0, 140, 40);
    app.tile_manager.toggle_inspector(area.width);
    app.tile_manager.render(area, &mut Buffer::empty(area));
    let inspector = app.tile_manager.find_pane(PaneKind::Inspector).unwrap();
    app.tile_manager.runtime.focus_pane(inspector).unwrap();
    let draft = app.chat_widget.composer_text_with_pending();
    for code in [
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Char('x'),
    ] {
        app.handle_tui_event(
            &mut tui,
            TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        )
        .await
        .unwrap();
        assert!(app.overlay.is_none());
    }
    app.handle_tui_event(&mut tui, TuiEvent::Paste("private clipboard".into()))
        .await
        .unwrap();
    let rect = app.tile_manager.pane_rect(inspector).unwrap();
    app.handle_tui_event(
        &mut tui,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: rect.x + 1,
            row: rect.y + 1,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await
    .unwrap();
    assert!(app.overlay.is_none());
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);

    // Escape and overlays cancel capture without closing/moving the Inspector
    // or leaking the subsequent release into a layout gesture.
    for overlay in [false, true] {
        let mut pointer = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + rect.width / 2,
            row: rect.y + rect.height / 2,
            modifiers: KeyModifiers::ALT,
        };
        app.handle_tui_event(&mut tui, TuiEvent::Mouse(pointer))
            .await
            .unwrap();
        if overlay {
            app.open_transcript_overlay(&mut tui, None);
        } else {
            app.handle_tui_event(
                &mut tui,
                TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            )
            .await
            .unwrap();
        }
        assert_eq!(app.tile_manager.focused(), Some(inspector));
        pointer.kind = MouseEventKind::Up(MouseButton::Left);
        pointer.column = 10;
        pointer.modifiers = KeyModifiers::NONE;
        app.handle_tui_event(&mut tui, TuiEvent::Mouse(pointer))
            .await
            .unwrap();
        if overlay {
            app.close_transcript_overlay(&mut tui);
            app.handle_tui_event(&mut tui, TuiEvent::Mouse(pointer))
                .await
                .unwrap();
        }
        app.tile_manager.render(area, &mut Buffer::empty(area));
        assert_eq!(app.tile_manager.pane_rect(inspector), Some(rect));
        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
    }

    // Moving dispatch before global keys must still honor plugin close requests.
    app.tile_manager
        .open_or_focus(PaneKind::ToolList, ratatui::layout::Direction::Horizontal);
    app.handle_key_event(
        &mut tui,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    )
    .await;
    assert!(app.tile_manager.find_pane(PaneKind::ToolList).is_none());
}
