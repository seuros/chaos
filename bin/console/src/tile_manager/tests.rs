use super::*;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

#[test]
fn inspector_split_preserves_chat_focus_and_respects_visibility() {
    let mut tiles = TileManager::new(
        Rc::new(RefCell::new(ToolListPane::new())),
        Rc::new(Cell::new(false)),
    );
    let area = Rect::new(3, 5, 140, 40);
    tiles.sync_inspector(area.width);
    assert!(tiles.is_single_pane(), "the inspector starts closed");
    assert!(tiles.chat_focused());
    tiles.runtime.open_palette().unwrap();
    assert!(tiles.uses_full_viewport());
    let mut buf = Buffer::empty(area);
    tiles.render(area, &mut buf);
    assert!(tiles.is_single_pane());
    assert!(!tiles.needs_inline_history_restore());
    let text: String = buf
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    for name in [PANE_CHAT, PANE_TOOL_LIST, PANE_INSPECTOR] {
        assert!(text.contains(name));
    }
    for name in ["block", PANE_MCP_ACTIVITY, PANE_MCP_MANAGEMENT] {
        assert!(!text.contains(name));
    }
    tiles.runtime.close_palette();
    assert!(tiles.needs_inline_history_restore());
    tiles.mark_inline_history_restored();
    assert!(!tiles.uses_full_viewport());
    tiles.toggle_inspector(area.width);
    tiles.render(area, &mut Buffer::empty(area));
    let inspector = tiles.find_pane(PaneKind::Inspector).expect("inspector");
    let chat_rect = tiles.pane_rect(PaneId::ROOT).expect("chat rect");
    let inspector_rect = tiles.pane_rect(inspector).expect("inspector rect");
    assert_eq!(tiles.focused(), Some(PaneId::ROOT));
    assert!(chat_rect.right() <= inspector_rect.x);
    assert!(chat_rect.width > inspector_rect.width);
    for _ in 0..2 {
        tiles.open_inspector(area.width);
        tiles.sync_inspector(area.width);
        assert_eq!(tiles.focused(), Some(inspector));
        assert!(!tiles.chat_focused());
        assert_eq!(tiles.find_pane(PaneKind::Inspector), Some(inspector));
        assert_eq!(tiles.pane_ids.len(), 2);
    }

    // Inspector navigation is handled by the registered plugin, not App fallbacks.
    tiles.runtime.focus_pane(inspector).unwrap();
    for code in [
        ratatui_hypertile::KeyCode::Up,
        ratatui_hypertile::KeyCode::PageDown,
        ratatui_hypertile::KeyCode::Home,
    ] {
        assert_eq!(
            tiles.handle_focused_plugin_key(KeyChord::new(code)),
            EventOutcome::Consumed
        );
    }
    assert_eq!(
        tiles.handle_focused_plugin_key(KeyChord::new(ratatui_hypertile::KeyCode::Char('x'))),
        EventOutcome::Ignored,
    );
    let mouse = |kind, rect: Rect| MouseEvent {
        kind,
        column: rect.x + 1,
        row: rect.y + 1,
        modifiers: KeyModifiers::NONE,
    };
    assert!(tiles.handle_pane_mouse(mouse(MouseEventKind::ScrollDown, inspector_rect)));
    assert!(tiles.handle_pane_mouse(mouse(MouseEventKind::Down(MouseButton::Left), chat_rect)));
    assert_eq!(tiles.focused(), Some(PaneId::ROOT));
    assert!(tiles.chat_focused());

    // Border capture resizes across redraws without passing input to plugins.
    let mut pointer = MouseEvent {
        column: inspector_rect.x,
        ..mouse(MouseEventKind::Down(MouseButton::Left), inspector_rect)
    };
    assert!(tiles.handle_pane_mouse(pointer));
    pointer.kind = MouseEventKind::Drag(MouseButton::Left);
    pointer.column -= 8;
    assert!(tiles.handle_pane_mouse(pointer));
    tiles.render(area, &mut Buffer::empty(area));
    assert!(tiles.pane_rect(PaneId::ROOT).unwrap().width < chat_rect.width);
    pointer.kind = MouseEventKind::Up(MouseButton::Left);
    assert!(tiles.handle_pane_mouse(pointer));
    assert_eq!(tiles.runtime.mode(), InputMode::PluginInput);

    // Ordinary content drags must not move panes. Alt capture swaps only on
    // release, even if the terminal no longer reports Alt after the press.
    for modifiers in [KeyModifiers::NONE, KeyModifiers::ALT] {
        let origin = tiles.pane_rect(inspector).unwrap();
        let target = tiles.pane_rect(PaneId::ROOT).unwrap();
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let down = kind == MouseEventKind::Down(MouseButton::Left);
            let rect = if down { origin } else { target };
            let handled = tiles.handle_pane_mouse(MouseEvent {
                kind,
                column: rect.x + rect.width / 2,
                row: rect.y + rect.height / 2,
                modifiers: if down { modifiers } else { KeyModifiers::NONE },
            });
            if modifiers == KeyModifiers::ALT {
                assert!(handled);
            }
            tiles.render(area, &mut Buffer::empty(area));
            let swapped =
                modifiers == KeyModifiers::ALT && kind == MouseEventKind::Up(MouseButton::Left);
            assert_eq!(
                tiles.pane_rect(inspector),
                Some(if swapped { target } else { origin })
            );
        }
        assert_eq!(tiles.find_pane(PaneKind::Chat), Some(PaneId::ROOT));
        assert_eq!(tiles.runtime.mode(), InputMode::PluginInput);
    }

    for code in [
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyCode::Tab,
    ] {
        tiles
            .runtime
            .focus_pane(inspector)
            .expect("focus inspector");
        assert!(tiles.leave_inspector(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE
        )));
        assert_eq!(tiles.focused(), Some(PaneId::ROOT));
        assert_eq!(tiles.find_pane(PaneKind::Inspector), Some(inspector));
    }

    tiles.sync_inspector(80);
    assert!(tiles.is_single_pane());
    assert!(tiles.needs_inline_history_restore());
    tiles.mark_inline_history_restored();
    assert!(!tiles.needs_inline_history_restore());
    tiles.sync_inspector(area.width);
    assert!(tiles.find_pane(PaneKind::Inspector).is_some());
    tiles.render(area, &mut Buffer::empty(area));
    tiles.toggle_inspector(area.width);
    tiles.sync_inspector(area.width);
    assert!(
        tiles.is_single_pane(),
        "a manually hidden panel must stay hidden"
    );
    assert!(tiles.needs_inline_history_restore());
    assert!(
        !tiles.handle_pane_mouse(pointer),
        "ignore stale tiled hit areas"
    );
}
