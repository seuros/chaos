use super::*;
use crate::auth::AuthCredentialsStoreMode;
use crate::auth::load_auth_dot_json;
use crate::auth::login_with_api_key;
use crate::auth::login_with_provider_api_key;
use pretty_assertions::assert_eq;

#[test]
fn accepts_known_provider_key_formats() {
    for (provider, key) in [
        ("openai", "sk-proj-test_key-123"),
        ("openai", "sk-svcacct-test_key-123"),
        ("openai", "sk-LegacyKey123"),
        ("openai", "sk-legacy_test-key"),
        ("anthropic", "sk-ant-api01-test_key-123"),
        ("anthropic", "sk-ant-api03-test_key-123"),
        ("anthropic", "sk-ant-api99-future-key-version"),
        ("charm", "sk-hyper-test-key"),
        ("moonshotai", "sk-MoonshotKey123"),
        ("moonshotai", "sk-opaque_test-key"),
        ("moonshotai-coding", "sk-kimi-test-key"),
        ("xai", "xai-test-key"),
    ] {
        assert_eq!(
            validate_provider_api_key(provider, key),
            Ok(()),
            "{provider}"
        );
    }
}

#[test]
fn rejects_other_provider_keys_without_treating_all_sk_keys_as_openai() {
    let keys = [
        ("openai", "sk-proj-openai-secret"),
        ("anthropic", "sk-ant-api03-anthropic-secret"),
        ("charm", "sk-hyper-hyper-secret"),
        ("moonshotai-coding", "sk-kimi-coding-secret"),
        ("xai", "xai-xai-secret"),
    ];
    for provider in [
        "openai",
        "anthropic",
        "charm",
        "moonshotai",
        "moonshotai-coding",
        "xai",
    ] {
        for (owner, key) in keys {
            assert_eq!(
                validate_provider_api_key(provider, key).is_ok(),
                provider == owner,
                "key owned by {owner} used for {provider}"
            );
        }
    }
}

#[test]
fn rejects_incomplete_keys_oauth_tokens_and_wrong_formats() {
    for (provider, key) in [
        ("openai", "sk-"),
        ("openai", "sk-proj-"),
        ("openai", "sk-svcacct-"),
        ("openai", "sk-ant-oat01-setup-token"),
        ("openai", "eyJhbGciOiJSUzI1NiJ9.payload.signature"),
        ("anthropic", "sk-ant-api03-"),
        ("anthropic", "sk-ant-api-secret"),
        ("anthropic", "sk-ant-oat01-setup-token"),
        ("anthropic", "sk-ant-admin01-admin-key"),
        ("charm", "sk-hyper-"),
        ("moonshotai", "sk-kimi-coding-secret"),
        ("moonshotai-coding", "sk-MoonshotKey123"),
        ("moonshotai-coding", "sk-kimi-"),
        ("moonshotai-coding", "SK-KIMI-uppercase-key"),
        ("xai", "xai-"),
    ] {
        assert!(
            matches!(
                validate_provider_api_key(provider, key),
                Err(ApiKeyValidationError::InvalidFormat { .. })
            ),
            "{provider}"
        );
    }
}

#[test]
fn custom_providers_have_no_builtin_prefix_rules_but_still_reject_empty_or_unsafe_input() {
    for provider in ["custom", "anthropic-proxy", "openai-compatible"] {
        for key in [
            "opaque-key",
            "id.secret",
            "sk-proj-proxied-key",
            "key+/=_-123",
        ] {
            assert_eq!(validate_provider_api_key(provider, key), Ok(()));
        }
    }
    for provider in ["custom", "openai", "anthropic", "moonshotai-coding"] {
        assert_eq!(
            validate_provider_api_key(provider, ""),
            Err(ApiKeyValidationError::Empty)
        );
        for key in [
            " ",
            "sk-kimi-with space",
            "sk-kimi-line\nbreak",
            "sk-kimi-tab\tkey",
            "sk-kimi-\0",
            "sk-kimi-\x1b",
            "sk-kimi-\u{200b}",
        ] {
            assert_eq!(
                validate_provider_api_key(provider, key),
                Err(ApiKeyValidationError::InvalidCharacters)
            );
        }
    }
}

#[test]
fn validation_errors_never_capture_or_display_the_secret() {
    let key = "sk-proj-DO_NOT_DISCLOSE_THIS_SECRET";
    let error = validate_provider_api_key("anthropic", key).unwrap_err();
    for message in [error.to_string(), format!("{error:?}")] {
        assert!(!message.contains(key));
        assert!(!message.contains("DO_NOT_DISCLOSE"));
    }
    assert!(error.to_string().contains("Anthropic"));
    assert!(error.to_string().contains("sk-ant-api"));
}

#[test]
fn invalid_key_does_not_create_a_record_or_access_the_store() {
    let home = tempfile::tempdir().unwrap();
    // Invalid JSON would fail a store read. Validation must happen before it.
    let path = home.path().join("auth.json");
    std::fs::write(&path, "not valid json").unwrap();
    let error = login_with_provider_api_key(
        home.path(),
        "anthropic",
        "sk-proj-wrong-provider",
        AuthCredentialsStoreMode::File,
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<ApiKeyValidationError>()
            .is_some()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "not valid json");

    let empty_home = tempfile::tempdir().unwrap();
    for mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let error = login_with_provider_api_key(
            empty_home.path(),
            "anthropic",
            "sk-proj-wrong-provider",
            mode,
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            load_auth_dot_json(empty_home.path(), mode)
                .unwrap()
                .is_none()
        );
    }
    assert!(!empty_home.path().join("auth.json").exists());
}

#[test]
fn rejected_update_preserves_all_credentials_and_valid_update_is_normalized() {
    for mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let home = tempfile::tempdir().unwrap();
        for (provider, key) in [
            ("openai", "sk-proj-original"),
            ("anthropic", "sk-ant-api03-original"),
            ("custom", "opaque-custom-key"),
        ] {
            login_with_provider_api_key(home.path(), provider, key, mode).unwrap();
        }
        let before = load_auth_dot_json(home.path(), mode).unwrap();
        let file_before = std::fs::read(home.path().join("auth.json")).ok();
        let error =
            login_with_provider_api_key(home.path(), "anthropic", "sk-proj-rejected-update", mode)
                .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(load_auth_dot_json(home.path(), mode).unwrap(), before);
        assert_eq!(
            std::fs::read(home.path().join("auth.json")).ok(),
            file_before
        );

        login_with_provider_api_key(
            home.path(),
            "anthropic",
            " \tsk-ant-api03-replacement\r\n",
            mode,
        )
        .unwrap();
        let after = load_auth_dot_json(home.path(), mode).unwrap().unwrap();
        assert_eq!(
            after
                .provider_record("anthropic")
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-ant-api03-replacement")
        );
        for provider in ["openai", "custom"] {
            assert_eq!(
                after.provider_record(provider),
                before.as_ref().unwrap().provider_record(provider)
            );
        }
    }
}

#[test]
fn legacy_login_entry_point_cannot_bypass_validation() {
    let home = tempfile::tempdir().unwrap();
    let error = login_with_api_key(
        home.path(),
        "sk-ant-api03-other-provider",
        AuthCredentialsStoreMode::File,
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!home.path().join("auth.json").exists());
}
