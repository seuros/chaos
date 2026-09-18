use super::*;

#[test]
fn inspector_split_preserves_chat_focus_and_respects_visibility() {
    let mut tiles = TileManager::new(
        Rc::new(RefCell::new(ToolListPane::new())),
        Rc::new(Cell::new(false)),
    );
    let area = Rect::new(0, 0, 140, 40);
    tiles.sync_inspector(area.width);
    assert!(tiles.is_single_pane(), "the inspector starts closed");
    tiles.toggle_inspector(area.width);
    tiles.render(area, &mut Buffer::empty(area));
    let inspector = tiles.find_pane(PaneKind::Inspector).expect("inspector");
    let chat_rect = tiles.pane_rect(PaneId::ROOT).expect("chat rect");
    let inspector_rect = tiles.pane_rect(inspector).expect("inspector rect");
    assert_eq!(tiles.focused(), Some(PaneId::ROOT));
    assert!(chat_rect.right() <= inspector_rect.x);
    assert!(chat_rect.width > inspector_rect.width);

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
}
