use super::*;

fn press(screen: &mut StorageScreen, code: KeyCode) -> Option<StorageChoice> {
    screen.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
}

fn rendered(screen: &StorageScreen, width: u16) -> String {
    let area = Rect::new(0, 0, width, 30);
    let mut buffer = Buffer::empty(area);
    screen.render(area, &mut buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn postgres_is_recommended_and_selected_by_default() {
    let mut screen = StorageScreen::default();
    let text = rendered(&screen, 80);
    assert!(text.contains(&format!("Welcome to {OS_NAME}")));
    assert!(text.contains(&format!("Where should {OS_NAME} store your data?")));
    assert!(text.contains("PostgreSQL (Recommended)"));
    assert!(text.contains("SQLite"));
    assert!(press(&mut screen, KeyCode::Enter).is_none());
    assert!(screen.page == Page::Postgres);
    assert!(press(&mut screen, KeyCode::Enter).is_none());
    assert!(screen.error.is_some());
}

#[test]
fn sqlite_requires_an_explicit_choice() {
    let mut screen = StorageScreen::default();
    press(&mut screen, KeyCode::Down);
    assert!(matches!(
        press(&mut screen, KeyCode::Enter),
        Some(StorageChoice::Sqlite)
    ));
    assert!(screen.page == Page::Connecting);
}

#[test]
fn pasted_connections_are_visible_and_q_is_input_not_quit() {
    let mut screen = StorageScreen::default();
    press(&mut screen, KeyCode::Enter);
    screen.handle_paste("  postgresql://user:secret-password@localhost/chaos\n".into());
    press(&mut screen, KeyCode::Char('q'));
    assert!(!screen.should_exit);
    assert!(screen.connection.ends_with("chaosq"));
    assert!(rendered(&screen, 80).contains("postgresql://user:secret-password@localhost/chaosq"));
    press(&mut screen, KeyCode::Backspace);
    let Some(StorageChoice::Postgres(connection)) = press(&mut screen, KeyCode::Enter) else {
        panic!("expected PostgreSQL setup");
    };
    assert_eq!(
        connection,
        "postgresql://user:secret-password@localhost/chaos"
    );
}

#[test]
fn cancellation_does_not_select_a_backend() {
    let mut screen = StorageScreen::default();
    press(&mut screen, KeyCode::Enter);
    assert!(press(&mut screen, KeyCode::Esc).is_none());
    assert!(screen.page == Page::Choose);
    assert!(!screen.should_exit);
    assert!(press(&mut screen, KeyCode::Esc).is_none());
    assert!(screen.should_exit);

    let mut screen = StorageScreen::default();
    assert!(
        screen
            .handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .is_none()
    );
    assert!(screen.should_exit);
}

#[test]
fn connection_failure_can_be_retried_or_changed_to_sqlite() {
    let mut screen = StorageScreen::default();
    press(&mut screen, KeyCode::Enter);
    screen.handle_paste("env:CHAOS_DATABASE_CONNECTION".into());
    assert!(press(&mut screen, KeyCode::Enter).is_some());
    screen.failed(anyhow::anyhow!("Cannot initialize the database."));
    assert!(screen.page == Page::Postgres);
    assert!(rendered(&screen, 80).contains("Cannot initialize"));
    assert!(press(&mut screen, KeyCode::Enter).is_some());
    // Escape while connecting drops the pending future in the event loop.
    press(&mut screen, KeyCode::Esc);
    press(&mut screen, KeyCode::Down);
    assert!(matches!(
        press(&mut screen, KeyCode::Enter),
        Some(StorageChoice::Sqlite)
    ));
}

#[test]
fn release_events_and_control_characters_do_not_submit() {
    let mut screen = StorageScreen::default();
    let mut key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    key.kind = KeyEventKind::Release;
    assert!(screen.handle_key_event(key).is_none());
    assert!(screen.page == Page::Choose);
    press(&mut screen, KeyCode::Enter);
    screen.handle_paste("env:DB\r\n\u{1b}".into());
    assert_eq!(screen.connection, "env:DB");
    screen.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(screen.connection.is_empty());
}
