//! Permission checking, account restriction enforcement, and disconnect helpers.

use std::path::Path;

use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthMode;
use crate::auth::DEFAULT_AUTH_PROVIDER_ID;
use crate::auth::load_auth;
use crate::auth::logout;
use crate::auth::storage::AuthDotJson;
use crate::auth::storage::ProviderAuthRecord;
use crate::config::Config;
use chaos_ipc::config_types::ForcedLoginMethod;

/// Enforce login method and workspace restrictions from the current config.
///
/// Logs out and returns an error if the active credentials violate any
/// configured constraint.
pub fn enforce_login_restrictions(config: &Config) -> std::io::Result<()> {
    let Some(auth) = load_auth(
        &config.chaos_home,
        /*enable_codex_api_key_env*/ true,
        config.cli_auth_credentials_store_mode,
    )?
    else {
        return Ok(());
    };

    if let Some(required_method) = config.forced_login_method {
        let method_violation = match (required_method, auth.auth_mode()) {
            (ForcedLoginMethod::Api, AuthMode::ApiKey) => None,
            (ForcedLoginMethod::Chatgpt, AuthMode::Chatgpt) => None,
            (ForcedLoginMethod::Api, AuthMode::Chatgpt) => Some(
                "API key login is required, but ChatGPT is currently being used. Logging out."
                    .to_string(),
            ),
            (ForcedLoginMethod::Chatgpt, AuthMode::ApiKey) => Some(
                "ChatGPT login is required, but an API key is currently being used. Logging out."
                    .to_string(),
            ),
        };

        if let Some(message) = method_violation {
            return logout_with_message(
                &config.chaos_home,
                message,
                config.cli_auth_credentials_store_mode,
            );
        }
    }

    if let Some(expected_account_id) = config.forced_chatgpt_workspace_id.as_deref() {
        if !auth.is_chatgpt_auth() {
            return Ok(());
        }

        let token_data = match auth.get_token_data() {
            Ok(data) => data,
            Err(err) => {
                return logout_with_message(
                    &config.chaos_home,
                    format!(
                        "Failed to load ChatGPT credentials while enforcing workspace restrictions: {err}. Logging out."
                    ),
                    config.cli_auth_credentials_store_mode,
                );
            }
        };

        // workspace is the external identifier for account id.
        let chatgpt_account_id = token_data.id_token.chatgpt_account_id.as_deref();
        if chatgpt_account_id != Some(expected_account_id) {
            let message = match chatgpt_account_id {
                Some(actual) => format!(
                    "Login is restricted to workspace {expected_account_id}, but current credentials belong to {actual}. Logging out."
                ),
                None => format!(
                    "Login is restricted to workspace {expected_account_id}, but current credentials lack a workspace identifier. Logging out."
                ),
            };
            return logout_with_message(
                &config.chaos_home,
                message,
                config.cli_auth_credentials_store_mode,
            );
        }
    }

    Ok(())
}

pub(super) fn logout_with_message(
    chaos_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active auth.
    let removal_result = logout_all_stores(chaos_home, auth_credentials_store_mode);
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove auth.json: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

pub(super) fn logout_all_stores(
    chaos_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return logout(chaos_home, AuthCredentialsStoreMode::Ephemeral);
    }
    let removed_ephemeral = logout(chaos_home, AuthCredentialsStoreMode::Ephemeral)?;
    let removed_managed = logout(chaos_home, auth_credentials_store_mode)?;
    Ok(removed_ephemeral || removed_managed)
}

fn disconnect_provider_in_store(
    chaos_home: &Path,
    provider_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    let Some(mut auth_dot_json) =
        super::load_auth_dot_json(chaos_home, auth_credentials_store_mode)?
    else {
        return Ok(false);
    };

    if auth_dot_json.provider_record(provider_id).is_none() {
        return Ok(false);
    }

    auth_dot_json.clear_provider_record(provider_id);
    if auth_dot_json.normalized_provider_records().is_empty() {
        return logout(chaos_home, auth_credentials_store_mode);
    }

    super::save_auth(chaos_home, &auth_dot_json, auth_credentials_store_mode)?;
    Ok(true)
}

/// Remove stored credentials for a single provider from every relevant store.
pub fn disconnect_provider_account(
    chaos_home: &Path,
    provider_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return disconnect_provider_in_store(
            chaos_home,
            provider_id,
            AuthCredentialsStoreMode::Ephemeral,
        );
    }

    let removed_ephemeral =
        disconnect_provider_in_store(chaos_home, provider_id, AuthCredentialsStoreMode::Ephemeral)?;
    let removed_managed =
        disconnect_provider_in_store(chaos_home, provider_id, auth_credentials_store_mode)?;
    Ok(removed_ephemeral || removed_managed)
}

/// Remove all stored provider credentials from every relevant store.
pub fn disconnect_all_provider_accounts(
    chaos_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    logout_all_stores(chaos_home, auth_credentials_store_mode)
}

/// Writes an `auth.json` that contains only the API key.
pub fn login_with_api_key(
    chaos_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    login_with_provider_api_key(
        chaos_home,
        DEFAULT_AUTH_PROVIDER_ID,
        api_key,
        auth_credentials_store_mode,
    )
}

/// Writes or updates a provider-scoped API key record inside `auth.json`.
pub fn login_with_provider_api_key(
    chaos_home: &Path,
    provider_id: &str,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    use chaos_ipc::api::AuthMode as ApiAuthMode;

    let mut auth_dot_json = super::load_auth_dot_json(chaos_home, auth_credentials_store_mode)?
        .unwrap_or(AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            providers: Default::default(),
        });
    auth_dot_json.set_provider_record(
        provider_id,
        ProviderAuthRecord {
            auth_mode: Some(ApiAuthMode::ApiKey),
            api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
        },
    );
    super::save_auth(chaos_home, &auth_dot_json, auth_credentials_store_mode)
}

/// Writes an in-memory auth payload for externally managed ChatGPT tokens.
pub fn login_with_chatgpt_auth_tokens(
    chaos_home: &Path,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson::from_external_access_token(
        access_token,
        chatgpt_account_id,
        chatgpt_plan_type,
    )?;
    super::save_auth(
        chaos_home,
        &auth_dot_json,
        AuthCredentialsStoreMode::Ephemeral,
    )
}
