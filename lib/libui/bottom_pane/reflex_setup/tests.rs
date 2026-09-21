use super::*;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::config::ConfigBuilder;

async fn form() -> (
    tempfile::TempDir,
    ReflexSetupForm,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::default()
        .chaos_home(home.path().to_path_buf())
        .fallback_cwd(Some(home.path().to_path_buf()))
        .build()
        .await
        .unwrap();
    let auth = Arc::new(AuthManager::new(
        home.path().to_path_buf(),
        false,
        AuthCredentialsStoreMode::Ephemeral,
    ));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (_, settings) = configuration::presets().into_iter().next().unwrap();
    let form = ReflexSetupForm::new(
        Arc::new(config),
        auth,
        None,
        "test".into(),
        settings,
        AppEventSender::new(tx),
    );
    (home, form, rx)
}

#[tokio::test]
async fn key_entry_is_masked_and_cancel_never_emits_it() {
    let (_home, mut form, mut rx) = form().await;
    form.focused = KEY;
    form.handle_paste("private-test-key".into());
    for (width, height) in [(100, 19), (40, 10), (10, 3), (1, 1)] {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        form.render(area, &mut buffer);
        let rendered: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(!rendered.contains("private-test-key"));
        if height >= 10 {
            assert!(rendered.contains("••••••••"));
        }
    }
    assert!(rx.try_recv().is_err(), "typing must not emit app events");
    form.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    form.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert_eq!(form.fields[KEY].text(), "private-test-key");
    form.on_ctrl_c();
    form.fields[KEY].input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
    assert!(form.complete);
    assert!(form.fields[KEY].text().is_empty());
    assert!(rx.try_recv().is_err(), "cancel must not save anything");
}

#[tokio::test]
async fn paste_and_validation_errors_never_echo_the_key() {
    let (_home, mut form, mut rx) = form().await;
    form.focused = KEY;
    form.handle_paste("private-test-key\nsecond-line".into());
    assert!(form.fields[KEY].text().is_empty());
    assert_eq!(form.error.as_deref(), Some("Paste a single-line value"));
    form.handle_paste("private-test-key".into());
    form.fields[TIMEOUT].insert_str("invalid");
    form.submit();
    assert!(!form.complete);
    assert_eq!(form.focused, TIMEOUT);
    assert!(!form.error.as_deref().unwrap().contains("private-test-key"));
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn submit_persists_local_settings_and_emits_only_saved_configuration() {
    let (_home, mut form, mut rx) = form().await;
    let (_, settings) = configuration::presets().into_iter().nth(2).unwrap();
    form.fields[URL] = settings.base_url.clone().unwrap().into();
    form.settings = settings;
    form.focused = KEY;
    form.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(form.complete);
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let AppEvent::ReflexSetupFinished { name, result, .. } = event else {
        panic!("expected saved settings, not a chat event");
    };
    let saved = result.unwrap();
    assert!(saved.api_key.is_none());
    let snapshot = chaos_kern::user_settings::snapshot(&form.config.chaos_home)
        .await
        .unwrap();
    let loaded: ReflexBackendSettings = snapshot.settings["reflex"][name]
        .clone()
        .try_into()
        .unwrap();
    assert_eq!(loaded, saved);
}
