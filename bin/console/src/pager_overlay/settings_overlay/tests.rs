use super::*;
use crate::app_event::AppEvent;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use chaos_kern::config::ReflexKind;
use crossterm::event::KeyModifiers;

fn rendered(overlay: &SettingsOverlay, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    buf.content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn accounts_and_settings_share_centered_frame_geometry() {
    let area = Rect::new(0, 0, 120, 40);
    for title in ["accounts", "reflex"] {
        let mut buf = Buffer::filled(area, ratatui::buffer::Cell::new("x"));
        let inner = render_settings_panel(area, &mut buf, title);
        assert_eq!(inner, Rect::new(18, 9, 84, 22));
        assert_eq!(buf[(16, 8)].symbol(), "╭");
        assert_eq!(buf[(103, 31)].symbol(), "╯");
        assert_eq!(buf[(0, 0)].symbol(), " ");
        let text: String = buf
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains(&format!("/ {title}")));
    }
    for area in [
        Rect::new(5, 7, 81, 23),
        Rect::new(5, 7, 40, 10),
        Rect::new(5, 7, 3, 2),
        Rect::new(5, 7, 1, 1),
        Rect::new(5, 7, 0, 0),
    ] {
        let mut buf = Buffer::filled(Rect::new(0, 0, 120, 40), ratatui::buffer::Cell::new("x"));
        let inner = render_settings_panel(area, &mut buf, "accounts");
        assert!(inner.is_empty() || inner.intersection(area) == inner);
        for (i, cell) in buf.content.iter().enumerate() {
            let (x, y) = buf.pos_of(i);
            if !area.contains((x, y).into()) {
                assert_eq!(cell.symbol(), "x", "settings panel escaped its area");
            }
        }
    }
}

#[tokio::test]
#[serial_test::serial]
async fn reflex_dialog_masks_keys_and_returns_to_picker_on_cancel() {
    let (chat, _tx, mut rx, mut ops) = make_chatwidget_manual_with_sender().await;
    while rx.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}

    for cancel in [
        KeyEvent::from(KeyCode::Esc),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        let mut overlay = SettingsOverlay::new("reflex", chat.reflex_picker_view());
        let picker = rendered(&overlay, 120, 40);
        for backend in ["typesafe", "openrouter", "minicheck", "shieldgemma"] {
            assert!(picker.contains(backend), "{backend} missing");
        }
        overlay.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(rx.try_recv().is_err());
        overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let AppEvent::OpenReflexSetup { name, settings } = rx.try_recv().unwrap() else {
            panic!("expected a form selection");
        };
        assert!(!overlay.is_done(), "selection must stay in the dialog");
        overlay.open_form(chat.reflex_setup_view(name.clone(), settings.clone()));
        overlay.open_form(chat.reflex_setup_view(name, settings));
        assert_eq!(overlay.views.len(), 2, "ignore duplicate selections");
        overlay.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert!(rendered(&overlay, 120, 40).contains("> Backend name"));
        for _ in 0..6 {
            overlay.handle_key_event(KeyEvent::from(KeyCode::Tab));
        }
        overlay.handle_paste("private-dialog-test-key".into());
        for (width, height) in [(120, 40), (80, 22), (40, 10), (3, 2), (1, 1), (0, 0)] {
            let text = rendered(&overlay, width, height);
            assert!(!text.contains("private-dialog-test-key"));
            if height >= 22 {
                assert!(text.contains("••••••••"));
            }
        }
        assert!(
            rx.try_recv().is_err(),
            "editing must not emit keys or history"
        );
        overlay.handle_key_event(cancel);
        assert_eq!(overlay.views.len(), 1);
        assert!(!overlay.is_done());
        assert_eq!(overlay.views[0].selected_index(), Some(0));
        assert!(rendered(&overlay, 120, 40).contains("Reflex backends"));
        overlay.handle_key_event(KeyEvent {
            kind: KeyEventKind::Repeat,
            ..cancel
        });
        assert!(!overlay.is_done(), "held cancel must not close the picker");
        overlay.handle_key_event(cancel);
        assert!(overlay.is_done());
        assert!(rx.try_recv().is_err(), "cancel must not save");
        assert!(
            ops.try_recv().is_err(),
            "settings must not submit a model turn"
        );
    }
}

#[tokio::test]
#[serial_test::serial]
async fn reflex_dialog_closes_after_submitting_local_settings() {
    let (chat, _tx, mut rx, mut ops) = make_chatwidget_manual_with_sender().await;
    while rx.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    let mut overlay = SettingsOverlay::new("reflex", chat.reflex_picker_view());
    for _ in 0..2 {
        overlay.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::OpenReflexSetup { name, settings } = rx.try_recv().unwrap() else {
        panic!("expected a form selection");
    };
    assert_eq!(settings.kind, ReflexKind::Minicheck);
    overlay.open_form(chat.reflex_setup_view(name, settings));
    for _ in 0..7 {
        overlay.handle_key_event(KeyEvent::from(KeyCode::Enter));
    }
    assert!(
        overlay.is_done(),
        "save must close rather than return to the picker"
    );
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let AppEvent::ReflexSetupFinished { result, .. } = event else {
        panic!("expected saved configuration");
    };
    let saved = result.unwrap();
    assert_eq!(saved.kind, ReflexKind::Minicheck);
    assert!(saved.api_key.is_none());
    assert!(ops.try_recv().is_err());
}
