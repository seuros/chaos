use super::*;
use crate::config::Config;
use crate::config::ConfigBuilder;
use crate::test_support::EnvVarGuard;
use crate::token_data::KnownPlan as InternalKnownPlan;
use crate::token_data::PlanType as InternalPlanType;
use chaos_ipc::account::PlanType as AccountPlanType;

use chaos_ipc::config_types::ForcedLoginMethod;
use jiff::Timestamp;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[allow(clippy::duplicate_mod)]
#[path = "test_support/auth_fixtures.rs"]
mod auth_test_fixtures;

use auth_test_fixtures::build_fake_jwt;
use auth_test_fixtures::openai_auth;
use auth_test_fixtures::parse_id_token;

#[tokio::test]
async fn refresh_without_id_token() {
    let chaos_home = tempdir().unwrap();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let storage = create_auth_storage(
        chaos_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
    );
    let updated = crate::auth::tokens::persist_tokens(
        &storage,
        DEFAULT_AUTH_PROVIDER_ID,
        None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
    )
    .expect("update_tokens should succeed");

    let tokens = updated
        .provider_record(DEFAULT_AUTH_PROVIDER_ID)
        .and_then(|record| record.tokens)
        .expect("tokens should exist");
    assert_eq!(tokens.id_token.raw_jwt, fake_jwt);
    assert_eq!(tokens.access_token, "new-access-token");
    assert_eq!(tokens.refresh_token, "new-refresh-token");
}

#[test]
fn login_with_api_key_overwrites_existing_auth_json() {
    let dir = tempdir().unwrap();
    let stale_auth = openai_auth(
        ApiAuthMode::Chatgpt,
        Some("sk-old"),
        Some(TokenData {
            id_token: parse_id_token(&build_fake_jwt(Some("pro"), None)),
            access_token: "stale-access".to_string(),
            refresh_token: "stale-refresh".to_string(),
            account_id: Some("stale-acc".to_string()),
        }),
        Some(Timestamp::now()),
    );
    super::save_auth(dir.path(), &stale_auth, AuthCredentialsStoreMode::File).unwrap();

    super::login_with_api_key(dir.path(), "sk-new", AuthCredentialsStoreMode::File)
        .expect("login_with_api_key should succeed");

    let auth = super::load_auth(dir.path(), false, AuthCredentialsStoreMode::File)
        .expect("auth load should succeed")
        .expect("auth should exist");
    assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
    assert_eq!(auth.api_key(), Some("sk-new"));
    assert!(
        auth.get_token_data().is_err(),
        "provider tokens should be cleared"
    );
}

#[test]
fn missing_auth_json_returns_none() {
    let dir = tempdir().unwrap();
    let auth = ChaosAuth::from_auth_storage(dir.path(), AuthCredentialsStoreMode::File)
        .expect("call should succeed");
    assert_eq!(auth, None);
}

#[tokio::test]
#[serial(codex_api_key)]
async fn pro_account_with_no_api_key_uses_chatgpt_auth() {
    let chaos_home = tempdir().unwrap();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(chaos_home.path(), false, AuthCredentialsStoreMode::File)
        .unwrap()
        .unwrap();
    assert_eq!(None, auth.api_key());
    assert_eq!(AuthMode::Chatgpt, auth.auth_mode());
    assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-12345"));

    let auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let provider_record = auth_dot_json
        .provider_record(DEFAULT_AUTH_PROVIDER_ID)
        .expect("openai provider record should exist");
    assert_eq!(provider_record.auth_mode, Some(ApiAuthMode::Chatgpt));
    assert_eq!(provider_record.api_key, None);
    assert!(provider_record.last_refresh.is_some());
    let tokens = provider_record.tokens.expect("chatgpt tokens should exist");
    assert_eq!(tokens.id_token.raw_jwt, fake_jwt);
    assert_eq!(tokens.id_token.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        tokens.id_token.chatgpt_plan_type,
        Some(InternalPlanType::Known(InternalKnownPlan::Pro))
    );
    assert_eq!(
        tokens.id_token.chatgpt_user_id.as_deref(),
        Some("user-12345")
    );
    assert_eq!(tokens.access_token, "test-access-token");
    assert_eq!(tokens.refresh_token, "test-refresh-token");
}

#[tokio::test]
#[serial(codex_api_key)]
async fn loads_api_key_from_auth_json() {
    let dir = tempdir().unwrap();
    let auth = openai_auth(ApiAuthMode::ApiKey, Some("sk-test-key"), None, None);
    super::save_auth(dir.path(), &auth, AuthCredentialsStoreMode::File).unwrap();

    let auth = super::load_auth(dir.path(), false, AuthCredentialsStoreMode::File)
        .unwrap()
        .unwrap();
    assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
    assert_eq!(auth.api_key(), Some("sk-test-key"));

    assert!(auth.get_token_data().is_err());
}

struct AuthFileParams {
    openai_api_key: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

fn write_auth_file(params: AuthFileParams, chaos_home: &Path) -> std::io::Result<String> {
    let fake_jwt = build_fake_jwt(
        params.chatgpt_plan_type.as_deref(),
        params.chatgpt_account_id.as_deref(),
    );
    let auth_mode = if params.openai_api_key.is_some() {
        ApiAuthMode::ApiKey
    } else {
        ApiAuthMode::Chatgpt
    };

    let auth = openai_auth(
        auth_mode,
        params.openai_api_key.as_deref(),
        Some(TokenData {
            id_token: parse_id_token(&fake_jwt),
            access_token: "test-access-token".to_string(),
            refresh_token: "test-refresh-token".to_string(),
            account_id: None,
        }),
        Some(Timestamp::now()),
    );
    super::save_auth(chaos_home, &auth, AuthCredentialsStoreMode::File)?;
    Ok(fake_jwt)
}

async fn build_config(
    chaos_home: &Path,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: Option<String>,
) -> Config {
    let mut config = ConfigBuilder::default()
        .chaos_home(chaos_home.to_path_buf())
        .build()
        .await
        .expect("config should load");
    config.forced_login_method = forced_login_method;
    config.forced_chatgpt_workspace_id = forced_chatgpt_workspace_id;
    config
}

#[tokio::test]
async fn enforce_login_restrictions_logs_out_for_method_mismatch() {
    let chaos_home = tempdir().unwrap();
    login_with_api_key(chaos_home.path(), "sk-test", AuthCredentialsStoreMode::File)
        .expect("seed api key");

    let config = build_config(chaos_home.path(), Some(ForcedLoginMethod::Chatgpt), None).await;

    let err =
        super::enforce_login_restrictions(&config).expect_err("expected method mismatch to error");
    assert!(err.to_string().contains("ChatGPT login is required"));
    assert!(
        !chaos_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_logs_out_for_workspace_mismatch() {
    let chaos_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_another_org".to_string()),
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(chaos_home.path(), None, Some("org_mine".to_string())).await;

    let err = super::enforce_login_restrictions(&config)
        .expect_err("expected workspace mismatch to error");
    assert!(err.to_string().contains("workspace org_mine"));
    assert!(
        !chaos_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_allows_matching_workspace() {
    let chaos_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("org_mine".to_string()),
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(chaos_home.path(), None, Some("org_mine".to_string())).await;

    super::enforce_login_restrictions(&config).expect("matching workspace should succeed");
    assert!(
        chaos_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
async fn enforce_login_restrictions_allows_api_key_if_login_method_not_set_but_forced_chatgpt_workspace_id_is_set()
 {
    let chaos_home = tempdir().unwrap();
    login_with_api_key(chaos_home.path(), "sk-test", AuthCredentialsStoreMode::File)
        .expect("seed api key");

    let config = build_config(chaos_home.path(), None, Some("org_mine".to_string())).await;

    super::enforce_login_restrictions(&config).expect("matching workspace should succeed");
    assert!(
        chaos_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
#[serial(codex_api_key)]
async fn enforce_login_restrictions_blocks_env_api_key_when_chatgpt_required() {
    let _guard = EnvVarGuard::set(CHAOS_API_KEY_ENV_VAR, "sk-env");
    let chaos_home = tempdir().unwrap();

    let config = build_config(chaos_home.path(), Some(ForcedLoginMethod::Chatgpt), None).await;

    let err = super::enforce_login_restrictions(&config)
        .expect_err("environment API key should not satisfy forced ChatGPT login");
    assert!(
        err.to_string()
            .contains("ChatGPT login is required, but an API key is currently being used.")
    );
}

#[test]
fn plan_type_maps_known_plan() {
    let chaos_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(chaos_home.path(), false, AuthCredentialsStoreMode::File)
        .expect("load auth")
        .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Pro));
}

#[test]
fn plan_type_maps_unknown_to_unknown() {
    let chaos_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("mystery-tier".to_string()),
            chatgpt_account_id: None,
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(chaos_home.path(), false, AuthCredentialsStoreMode::File)
        .expect("load auth")
        .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}

#[test]
fn missing_plan_type_maps_to_unknown() {
    let chaos_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: None,
            chatgpt_account_id: None,
        },
        chaos_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(chaos_home.path(), false, AuthCredentialsStoreMode::File)
        .expect("load auth")
        .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}
