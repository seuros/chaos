use super::*;
use chaos_kern::auth::load_auth_dot_json;
use pretty_assertions::assert_eq;

fn anthropic_entry_widget() -> (AccountsWidget, TempDir) {
    let (mut widget, home) = widget_with_model_providers(built_in_model_providers());
    // Keep tests independent of the developer's exported credentials.
    for provider in &mut widget.providers {
        provider.env_key = None;
    }
    let index = widget
        .providers
        .iter()
        .position(|provider| provider.id == "anthropic")
        .unwrap();
    widget.select_provider_by_index(index);
    assert!(matches!(
        widget.sign_in_state(),
        SignInState::ApiKeyEntry(_)
    ));
    (widget, home)
}

#[test]
fn accounts_rejects_wrong_provider_key_without_switching_and_allows_correction() {
    let (mut widget, home) = anthropic_entry_widget();
    let wrong_key = "sk-proj-wrong-provider-secret";
    widget.handle_paste(wrong_key.into());
    widget.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let SignInState::ApiKeyEntry(state) = widget.sign_in_state() else {
        panic!("rejected keys must keep the account entry open");
    };
    assert_eq!(state.provider.as_ref().unwrap().id, "anthropic");
    assert_eq!(state.value, wrong_key);
    assert_eq!(widget.selected_provider().unwrap().id, "anthropic");
    assert!(widget.completion().is_none());
    assert!(!home.path().join("auth.json").exists());
    let error = widget.error_message().unwrap();
    assert!(error.contains("Invalid API key for Anthropic"));
    assert!(!error.contains(wrong_key));

    let area = Rect::new(0, 0, 72, 16);
    let mut buf = Buffer::empty(area);
    widget.render_api_key_entry(area, &mut buf, &state);
    assert!(buffer_to_text(&buf, area).contains("Invalid API key for Anthropic"));

    for _ in wrong_key.chars() {
        widget.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    }
    let corrected = "sk-ant-api03-corrected-key";
    widget.handle_paste(corrected.into());
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        widget.completion(),
        Some(AccountsCompletion::ConnectedProvider {
            provider_id: "anthropic".into()
        })
    );
    assert!(widget.error_message().is_none());
    assert_eq!(
        load_auth_dot_json(home.path(), AuthCredentialsStoreMode::File)
            .unwrap()
            .unwrap()
            .provider_record("anthropic")
            .unwrap()
            .api_key
            .as_deref(),
        Some(corrected)
    );
}

#[test]
fn accounts_rejected_update_preserves_existing_credentials() {
    let (mut widget, home) = anthropic_entry_widget();
    for (provider, key) in [
        ("anthropic", "sk-ant-api03-original"),
        ("openai", "sk-proj-original"),
    ] {
        login_with_provider_api_key(home.path(), provider, key, AuthCredentialsStoreMode::File)
            .unwrap();
    }
    let path = home.path().join("auth.json");
    let before = std::fs::read(&path).unwrap();

    widget.handle_paste("sk-kimi-wrong-provider".into());
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(widget.selected_provider().unwrap().id, "anthropic");
    assert!(widget.completion().is_none());
    assert!(widget.error_message().is_some());
}

#[test]
fn accounts_custom_provider_accepts_its_own_key_format() {
    let mut custom =
        chaos_kern::create_oss_provider_with_base_url("https://custom.example/v1", WireApi::Auto);
    custom.env_key = Some("CUSTOM_KEY".into());
    let (mut widget, home) =
        widget_with_model_providers(HashMap::from([("custom".into(), custom)]));
    widget.providers[0].env_key = None;
    widget.select_provider_by_index(0);
    widget.handle_paste("opaque-custom-key".into());
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        widget.completion(),
        Some(AccountsCompletion::ConnectedProvider {
            provider_id: "custom".into()
        })
    );
    assert!(
        load_auth_dot_json(home.path(), AuthCredentialsStoreMode::File)
            .unwrap()
            .unwrap()
            .provider_record("custom")
            .is_some()
    );
}

#[test]
fn accounts_picker_does_not_autodetect_pasted_keys() {
    let (mut widget, home) = widget_with_model_providers(built_in_model_providers());
    widget.handle_paste("sk-kimi-do-not-autodetect".into());
    assert!(matches!(widget.sign_in_state(), SignInState::PickProvider));
    assert!(widget.selected_provider_id.read().unwrap().is_none());
    assert!(!home.path().join("auth.json").exists());
}
