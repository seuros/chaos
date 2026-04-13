//! Token storage, rotation, and validation logic.
//!
//! This module owns the `AuthManager` type and the low-level helpers for
//! persisting and refreshing ChatGPT OAuth tokens.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use crate::auth::AuthCredentialsStoreMode;
use crate::auth::ChaosAuth;
use crate::auth::ChatgptAuth;
use crate::auth::ExternalAuthRefreshContext;
use crate::auth::ExternalAuthRefreshReason;
use crate::auth::ExternalAuthRefresher;
use crate::auth::RefreshTokenError;
use crate::auth::load_auth;
use crate::auth::save_auth;
use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthStorageBackend;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::util::try_parse_error_message;
use codex_client::ChaosHttpClient;
use http::StatusCode;
use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;

use super::permissions::logout_all_stores;

const REFRESH_TOKEN_EXPIRED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.";
const REFRESH_TOKEN_REUSED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) use super::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;

// Shared constant for token refresh (client id used for oauth token refresh flow)
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Serialize)]
pub(super) struct RefreshRequest {
    pub(super) client_id: &'static str,
    pub(super) grant_type: &'static str,
    pub(super) refresh_token: String,
}

#[derive(Deserialize, Clone)]
pub(super) struct RefreshResponse {
    pub(super) id_token: Option<String>,
    pub(super) access_token: Option<String>,
    pub(super) refresh_token: Option<String>,
}

pub(super) fn refresh_token_endpoint() -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| REFRESH_TOKEN_URL.to_string())
}

/// Persist refreshed tokens into auth storage and update last_refresh.
pub(crate) fn persist_tokens(
    storage: &Arc<dyn AuthStorageBackend>,
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
) -> std::io::Result<AuthDotJson> {
    let mut auth_dot_json = storage
        .load()?
        .ok_or(std::io::Error::other("Token data is not available."))?;

    let tokens = auth_dot_json.tokens.get_or_insert_with(TokenData::default);
    if let Some(id_token) = id_token {
        tokens.id_token = parse_chatgpt_jwt_claims(&id_token).map_err(std::io::Error::other)?;
    }
    if let Some(access_token) = access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth_dot_json.last_refresh = Some(Timestamp::now());
    storage.save(&auth_dot_json)?;
    Ok(auth_dot_json)
}

/// Requests refreshed ChatGPT OAuth tokens from the auth service using a refresh token.
pub(super) async fn request_chatgpt_token_refresh(
    refresh_token: String,
    client: &ChaosHttpClient,
) -> Result<RefreshResponse, RefreshTokenError> {
    let refresh_request = RefreshRequest {
        client_id: CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
    };

    let endpoint = refresh_token_endpoint();

    let response = client
        .post(endpoint.as_str())
        .header("Content-Type", "application/json")
        .json(&refresh_request)
        .send()
        .await
        .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;

    let status = response.status();
    if status.is_success() {
        let refresh_response = response
            .json::<RefreshResponse>()
            .await
            .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;
        Ok(refresh_response)
    } else {
        let body = response.text().await.unwrap_or_default();
        tracing::error!("Failed to refresh token: {status}: {body}");
        if status == StatusCode::UNAUTHORIZED {
            let failed = classify_refresh_token_failure(&body);
            Err(RefreshTokenError::Permanent(failed))
        } else {
            let message = try_parse_error_message(&body);
            Err(RefreshTokenError::Transient(std::io::Error::other(
                format!("Failed to refresh token: {status}: {message}"),
            )))
        }
    }
}

pub(super) fn classify_refresh_token_failure(body: &str) -> RefreshTokenFailedError {
    let code = extract_refresh_token_error_code(body);

    let normalized_code = code.as_deref().map(str::to_ascii_lowercase);
    let reason = match normalized_code.as_deref() {
        Some("refresh_token_expired") => RefreshTokenFailedReason::Expired,
        Some("refresh_token_reused") => RefreshTokenFailedReason::Exhausted,
        Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
        _ => RefreshTokenFailedReason::Other,
    };

    if reason == RefreshTokenFailedReason::Other {
        tracing::warn!(
            backend_code = normalized_code.as_deref(),
            backend_body = body,
            "Encountered unknown 401 response while refreshing token"
        );
    }

    let message = match reason {
        RefreshTokenFailedReason::Expired => REFRESH_TOKEN_EXPIRED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Exhausted => REFRESH_TOKEN_REUSED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Revoked => REFRESH_TOKEN_INVALIDATED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Other => REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
    };

    RefreshTokenFailedError::new(reason, message)
}

fn extract_refresh_token_error_code(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }

    let serde_json::Value::Object(map) = serde_json::from_str::<serde_json::Value>(body).ok()?
    else {
        return None;
    };

    if let Some(error_value) = map.get("error") {
        match error_value {
            serde_json::Value::Object(obj) => {
                if let Some(code) = obj.get("code").and_then(serde_json::Value::as_str) {
                    return Some(code.to_string());
                }
            }
            serde_json::Value::String(code) => {
                return Some(code.to_string());
            }
            _ => {}
        }
    }

    map.get("code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Internal cached auth state.
#[derive(Clone)]
pub(super) struct CachedAuth {
    pub(super) auth: Option<ChaosAuth>,
    /// Callback used to refresh external auth by asking the parent app for new tokens.
    pub(super) external_refresher: Option<Arc<dyn ExternalAuthRefresher>>,
}

impl std::fmt::Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(ChaosAuth::api_auth_mode),
            )
            .field(
                "external_refresher",
                &self.external_refresher.as_ref().map(|_| "present"),
            )
            .finish()
    }
}

pub(super) enum ReloadOutcome {
    /// Reload was performed and the cached auth changed
    ReloadedChanged,
    /// Reload was performed and the cached auth remained the same
    ReloadedNoChange,
    /// Reload was skipped (missing or mismatched account id)
    Skipped,
}

/// Central manager providing a single source of truth for auth.json derived
/// authentication data. It loads once (or on preference change) and then
/// hands out cloned `ChaosAuth` values so the rest of the program has a
/// consistent snapshot.
///
/// External modifications to `auth.json` will NOT be observed until
/// `reload()` is called explicitly. This matches the design goal of avoiding
/// different parts of the program seeing inconsistent auth data mid-run.
#[derive(Debug)]
pub struct AuthManager {
    pub(super) chaos_home: PathBuf,
    pub(super) inner: RwLock<CachedAuth>,
    pub(super) enable_codex_api_key_env: bool,
    pub(super) auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub(super) forced_chatgpt_workspace_id: RwLock<Option<String>>,
}

impl AuthManager {
    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state.
    pub fn new(
        chaos_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        let managed_auth = load_auth(
            &chaos_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        )
        .ok()
        .flatten();
        Self {
            chaos_home,
            inner: RwLock::new(CachedAuth {
                auth: managed_auth,
                external_refresher: None,
            }),
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            forced_chatgpt_workspace_id: RwLock::new(None),
        }
    }

    /// Create an AuthManager with a specific ChaosAuth, for testing only.
    pub(crate) fn from_auth_for_testing(auth: ChaosAuth) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            external_refresher: None,
        };

        Arc::new(Self {
            chaos_home: PathBuf::from("non-existent"),
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
        })
    }

    /// Create an AuthManager with a specific ChaosAuth and chaos home, for testing only.
    pub(crate) fn from_auth_for_testing_with_home(
        auth: ChaosAuth,
        chaos_home: PathBuf,
    ) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            external_refresher: None,
        };
        Arc::new(Self {
            chaos_home,
            inner: RwLock::new(cached),
            enable_codex_api_key_env: false,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            forced_chatgpt_workspace_id: RwLock::new(None),
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    pub fn auth_cached(&self) -> Option<ChaosAuth> {
        self.inner.read().ok().and_then(|c| c.auth.clone())
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    /// Refreshes cached ChatGPT tokens if they are stale before returning.
    pub async fn auth(&self) -> Option<ChaosAuth> {
        let auth = self.auth_cached()?;
        if let Err(err) = self.refresh_if_stale(&auth).await {
            tracing::error!("Failed to refresh token: {}", err);
            return Some(auth);
        }
        self.auth_cached()
    }

    /// Force a reload of the auth information from auth.json. Returns
    /// whether the auth value changed.
    pub fn reload(&self) -> bool {
        tracing::info!("Reloading auth");
        let new_auth = self.load_auth_from_storage();
        self.set_cached_auth(new_auth)
    }

    pub(super) fn reload_if_account_id_matches(
        &self,
        expected_account_id: Option<&str>,
    ) -> ReloadOutcome {
        let expected_account_id = match expected_account_id {
            Some(account_id) => account_id,
            None => {
                tracing::info!("Skipping auth reload because no account id is available.");
                return ReloadOutcome::Skipped;
            }
        };

        let new_auth = self.load_auth_from_storage();
        let new_account_id = new_auth.as_ref().and_then(ChaosAuth::get_account_id);

        if new_account_id.as_deref() != Some(expected_account_id) {
            let found_account_id = new_account_id.as_deref().unwrap_or("unknown");
            tracing::info!(
                "Skipping auth reload due to account id mismatch (expected: {expected_account_id}, found: {found_account_id})"
            );
            return ReloadOutcome::Skipped;
        }

        tracing::info!("Reloading auth for account {expected_account_id}");
        let cached_before_reload = self.auth_cached();
        let auth_changed =
            !Self::auths_equal_for_refresh(cached_before_reload.as_ref(), new_auth.as_ref());
        self.set_cached_auth(new_auth);
        if auth_changed {
            ReloadOutcome::ReloadedChanged
        } else {
            ReloadOutcome::ReloadedNoChange
        }
    }

    fn auths_equal_for_refresh(a: Option<&ChaosAuth>, b: Option<&ChaosAuth>) -> bool {
        use chaos_ipc::api::AuthMode as ApiAuthMode;
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (ApiAuthMode::ApiKey, ApiAuthMode::ApiKey) => a.api_key() == b.api_key(),
                (ApiAuthMode::Chatgpt, ApiAuthMode::Chatgpt)
                | (ApiAuthMode::ChatgptAuthTokens, ApiAuthMode::ChatgptAuthTokens) => {
                    a.get_current_auth_json() == b.get_current_auth_json()
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn auths_equal(a: Option<&ChaosAuth>, b: Option<&ChaosAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn load_auth_from_storage(&self) -> Option<ChaosAuth> {
        load_auth(
            &self.chaos_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
        )
        .ok()
        .flatten()
    }

    fn set_cached_auth(&self, new_auth: Option<ChaosAuth>) -> bool {
        if let Ok(mut guard) = self.inner.write() {
            let previous = guard.auth.as_ref();
            let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
            tracing::info!("Reloaded auth, changed: {changed}");
            guard.auth = new_auth;
            changed
        } else {
            false
        }
    }

    pub fn set_external_auth_refresher(&self, refresher: Arc<dyn ExternalAuthRefresher>) {
        if let Ok(mut guard) = self.inner.write() {
            guard.external_refresher = Some(refresher);
        }
    }

    pub fn clear_external_auth_refresher(&self) {
        if let Ok(mut guard) = self.inner.write() {
            guard.external_refresher = None;
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<String>) {
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write() {
            *guard = workspace_id;
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<String> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn has_external_auth_refresher(&self) -> bool {
        self.inner
            .read()
            .ok()
            .map(|guard| guard.external_refresher.is_some())
            .unwrap_or(false)
    }

    pub fn is_external_auth_active(&self) -> bool {
        self.auth_cached()
            .as_ref()
            .is_some_and(ChaosAuth::is_external_chatgpt_tokens)
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub fn shared(
        chaos_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            chaos_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        ))
    }

    pub fn unauthorized_recovery(self: &Arc<Self>) -> super::policies::UnauthorizedRecovery {
        super::policies::UnauthorizedRecovery::new(Arc::clone(self))
    }

    /// Attempt to refresh the token by first performing a guarded reload.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        let auth_before_reload = self.auth_cached();
        let expected_account_id = auth_before_reload
            .as_ref()
            .and_then(ChaosAuth::get_account_id);

        match self.reload_if_account_id_matches(expected_account_id.as_deref()) {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_token_from_authority().await,
            ReloadOutcome::Skipped => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    RefreshTokenFailedReason::Other,
                    REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                )))
            }
        }
    }

    /// Attempt to refresh the current auth token from the authority that issued
    /// the token.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let auth = match self.auth_cached() {
            Some(auth) => auth,
            None => return Ok(()),
        };
        match auth {
            ChaosAuth::ChatgptAuthTokens(_) => {
                self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                    .await
            }
            ChaosAuth::Chatgpt(chatgpt_auth) => {
                let token_data = chatgpt_auth.current_token_data().ok_or_else(|| {
                    RefreshTokenError::Transient(std::io::Error::other(
                        "Token data is not available.",
                    ))
                })?;
                self.refresh_and_persist_chatgpt_token(&chatgpt_auth, token_data.refresh_token)
                    .await?;
                Ok(())
            }
            ChaosAuth::ApiKey(_) => Ok(()),
        }
    }

    /// Log out by deleting the on-disk auth.json (if present).
    pub fn logout(&self) -> std::io::Result<bool> {
        let removed = logout_all_stores(&self.chaos_home, self.auth_credentials_store_mode)?;
        // Always reload to clear any cached auth (even if file absent).
        self.reload();
        Ok(removed)
    }

    pub fn get_api_auth_mode(&self) -> Option<chaos_ipc::api::AuthMode> {
        self.auth_cached().as_ref().map(ChaosAuth::api_auth_mode)
    }

    pub fn auth_mode(&self) -> Option<super::AuthMode> {
        self.auth_cached().as_ref().map(ChaosAuth::auth_mode)
    }

    pub(super) async fn refresh_if_stale(
        &self,
        auth: &ChaosAuth,
    ) -> Result<bool, RefreshTokenError> {
        use jiff::ToSpan;

        let chatgpt_auth = match auth {
            ChaosAuth::Chatgpt(chatgpt_auth) => chatgpt_auth,
            _ => return Ok(false),
        };

        let auth_dot_json = match chatgpt_auth.current_auth_json() {
            Some(auth_dot_json) => auth_dot_json,
            None => return Ok(false),
        };
        let tokens = match auth_dot_json.tokens {
            Some(tokens) => tokens,
            None => return Ok(false),
        };
        let last_refresh = match auth_dot_json.last_refresh {
            Some(last_refresh) => last_refresh,
            None => return Ok(false),
        };

        const TOKEN_REFRESH_INTERVAL: i64 = 8;
        if last_refresh
            >= Timestamp::now()
                .checked_sub(TOKEN_REFRESH_INTERVAL.saturating_mul(24).hours())
                .unwrap_or(Timestamp::now())
        {
            return Ok(false);
        }
        self.refresh_and_persist_chatgpt_token(chatgpt_auth, tokens.refresh_token)
            .await?;
        Ok(true)
    }

    pub(super) async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let forced_chatgpt_workspace_id = self.forced_chatgpt_workspace_id();
        let refresher = match self.inner.read() {
            Ok(guard) => guard.external_refresher.clone(),
            Err(_) => {
                return Err(RefreshTokenError::Transient(std::io::Error::other(
                    "failed to read external auth state",
                )));
            }
        };

        let Some(refresher) = refresher else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth refresher is not configured",
            )));
        };

        let previous_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(ChaosAuth::get_account_id);
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = refresher.refresh(context).await?;
        if let Some(expected_workspace_id) = forced_chatgpt_workspace_id.as_deref()
            && refreshed.chatgpt_account_id != expected_workspace_id
        {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                format!(
                    "external auth refresh returned workspace {:?}, expected {expected_workspace_id:?}",
                    refreshed.chatgpt_account_id,
                ),
            )));
        }
        let auth_dot_json =
            AuthDotJson::from_external_tokens(&refreshed).map_err(RefreshTokenError::Transient)?;
        save_auth(
            &self.chaos_home,
            &auth_dot_json,
            AuthCredentialsStoreMode::Ephemeral,
        )
        .map_err(RefreshTokenError::Transient)?;
        self.reload();
        Ok(())
    }

    pub(super) async fn refresh_and_persist_chatgpt_token(
        &self,
        auth: &ChatgptAuth,
        refresh_token: String,
    ) -> Result<(), RefreshTokenError> {
        let refresh_response = request_chatgpt_token_refresh(refresh_token, auth.client()).await?;

        persist_tokens(
            auth.storage(),
            refresh_response.id_token,
            refresh_response.access_token,
            refresh_response.refresh_token,
        )
        .map_err(RefreshTokenError::from)?;
        self.reload();

        Ok(())
    }
}
