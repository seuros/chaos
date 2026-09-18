use super::*;
use chaos_kern::auth::disconnect_provider_account;
use chaos_kern::auth::load_auth_dot_json;
use pretty_assertions::assert_eq;

const PROVIDERS: [(&str, &str, &str, &str); 2] = [
    (
        "moonshotai",
        "Moonshot AI",
        "MOONSHOT_API_KEY",
        "https://platform.kimi.ai/console/api-keys",
    ),
    (
        "moonshotai-coding",
        "Moonshot AI Coding",
        "KIMI_API_KEY",
        "https://www.kimi.com/code/console",
    ),
];

fn manual_entry_widget() -> (AccountsWidget, TempDir) {
    let (mut widget, home) = widget_with_model_providers(built_in_model_providers());
    // Exercise manual entry without reading the developer's exported API keys.
    // Registry-to-widget environment metadata is checked separately below.
    for provider in &mut widget.providers {
        provider.env_key = None;
    }
    (widget, home)
}

fn provider_index(widget: &AccountsWidget, id: &str) -> usize {
    widget
        .providers
        .iter()
        .position(|provider| provider.id == id)
        .expect("Moonshot AI provider should be connectable")
}

#[test]
fn moonshotai_accounts_use_registry_auth_metadata_and_login_policy() {
    let registry = built_in_model_providers();
    for forced_login_method in [None, Some(ForcedLoginMethod::Api)] {
        let providers = AccountsWidget::build_connectable_providers(&registry, forced_login_method);
        for (id, name, env_key, console_url) in PROVIDERS {
            let provider = providers.iter().find(|provider| provider.id == id).unwrap();
            assert_eq!(provider.display_name, name);
            assert_eq!(provider.env_key.as_deref(), Some(env_key));
            assert_eq!(
                provider.env_key_instructions,
                registry[id].env_key_instructions
            );
            assert!(
                provider
                    .env_key_instructions
                    .as_ref()
                    .unwrap()
                    .contains(console_url)
            );
            assert!(provider.supports_api_key);
            assert!(!provider.supports_chatgpt_account);
            assert!(!provider.supports_xai_account);
        }
    }
    let restricted =
        AccountsWidget::build_connectable_providers(&registry, Some(ForcedLoginMethod::Chatgpt));
    for (id, ..) in PROVIDERS {
        assert!(!restricted.iter().any(|provider| provider.id == id));
    }
}

#[test]
fn moonshotai_accounts_render_and_open_by_number_with_key_guidance() {
    let (mut widget, _home) = manual_entry_widget();
    for (id, name, env_key, console_url) in PROVIDERS {
        let index = provider_index(&widget, id);
        widget.highlighted_provider = index;
        let picker_area = Rect::new(0, 0, 72, 14);
        let mut picker = Buffer::empty(picker_area);
        widget.render_pick_provider(picker_area, &mut picker);
        assert!(buffer_to_text(&picker, picker_area).contains(&format!("{}. {name}", index + 1)));

        let shortcut = char::from_digit((index + 1) as u32, 10).unwrap();
        widget.handle_key_event(KeyEvent::new(KeyCode::Char(shortcut), KeyModifiers::NONE));
        let SignInState::ApiKeyEntry(mut state) = widget.sign_in_state() else {
            panic!("Selecting {id} should open API-key entry");
        };
        assert_eq!(state.provider.as_ref().unwrap().id, id);
        assert_eq!(
            widget.displayed_sign_in_options(),
            vec![SignInOption::ApiKey]
        );

        let entry_area = Rect::new(0, 0, 72, 16);
        let mut entry = Buffer::empty(entry_area);
        widget.render_api_key_entry(entry_area, &mut entry, &state);
        let text = buffer_to_text(&entry, entry_area);
        assert!(
            text.contains(&format!("Use your own {name} API key")),
            "{text}"
        );
        assert!(text.contains(console_url), "{text}");
        assert!(text.contains(env_key), "{text}");
        assert!(text.contains("Press Enter to save"), "{text}");
        assert!(text.contains("Press Esc to go back"), "{text}");

        state.prepopulated_from_env = true;
        state.provider.as_mut().unwrap().env_key = Some(env_key.into());
        widget.set_error(Some("Failed to save API key: permission denied".into()));
        let mut entry = Buffer::empty(entry_area);
        widget.render_api_key_entry(entry_area, &mut entry, &state);
        let text = buffer_to_text(&entry, entry_area);
        assert!(
            text.contains("Failed to save API key: permission denied"),
            "{text}"
        );

        widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(widget.sign_in_state(), SignInState::PickProvider));
    }
}

#[test]
fn moonshotai_accounts_back_then_arrow_uses_new_provider_and_clears_draft_key() {
    let (mut widget, _home) = manual_entry_widget();
    widget.highlighted_provider = provider_index(&widget, "moonshotai");
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    widget.handle_paste("sk-metered-draft".into());
    widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    let coding_index = provider_index(&widget, "moonshotai-coding");
    let steps = (coding_index + widget.providers.len() - widget.highlighted_provider)
        % widget.providers.len();
    for _ in 0..steps {
        widget.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(widget.highlighted_provider, coding_index);
    widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let SignInState::ApiKeyEntry(state) = widget.sign_in_state() else {
        panic!("Selecting the coding provider should open API-key entry");
    };
    assert_eq!(state.provider.unwrap().id, "moonshotai-coding");
    assert!(
        state.value.is_empty(),
        "Do not reuse the metered API-key draft"
    );
    assert!(widget.completion().is_none());
}

#[test]
fn moonshotai_accounts_save_reload_and_disconnect_credentials_independently() {
    let (mut widget, home) = manual_entry_widget();
    login_with_provider_api_key(
        home.path(),
        "openai",
        "sk-proj-existing-openai",
        AuthCredentialsStoreMode::File,
    )
    .unwrap();
    for (id, key) in [
        ("moonshotai", "sk-meteredtest"),
        ("moonshotai-coding", "sk-kimi-coding-test"),
        ("moonshotai", "sk-meteredreplaced"),
    ] {
        *widget.sign_in_state.write().unwrap() = SignInState::PickProvider;
        widget.highlighted_provider = provider_index(&widget, id);
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            widget.sign_in_state(),
            SignInState::ApiKeyEntry(_)
        ));
        widget.handle_paste(format!("  {key}\n"));
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            widget.completion(),
            Some(AccountsCompletion::ConnectedProvider {
                provider_id: id.into(),
            })
        );
        let auth = widget.auth_manager.auth_for_provider(id).unwrap();
        assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
        assert_eq!(auth.api_key(), Some(key));
    }
    let stored = load_auth_dot_json(home.path(), AuthCredentialsStoreMode::File)
        .unwrap()
        .unwrap();
    assert_eq!(stored.providers.len(), 3);
    for (id, expected) in [
        ("openai", "sk-proj-existing-openai"),
        ("moonshotai", "sk-meteredreplaced"),
        ("moonshotai-coding", "sk-kimi-coding-test"),
    ] {
        let record = stored.provider_record(id).unwrap();
        assert_eq!(record.api_key.as_deref(), Some(expected));
        assert!(record.tokens.is_none());
    }
    assert!(
        disconnect_provider_account(home.path(), "moonshotai", AuthCredentialsStoreMode::File)
            .unwrap()
    );
    assert!(
        widget
            .auth_manager
            .auth_for_provider("moonshotai")
            .is_none()
    );
    assert_eq!(
        widget
            .auth_manager
            .auth_for_provider("moonshotai-coding")
            .unwrap()
            .api_key(),
        Some("sk-kimi-coding-test")
    );
}
