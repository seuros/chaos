pub(crate) mod permissions;
pub(crate) mod policies;
mod storage;
pub(crate) mod tokens;

use jiff::Timestamp;
#[cfg(test)]
use serial_test::serial;
use std::env;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use chaos_ipc::api::AuthMode as ApiAuthMode;
use chaos_syslog::TelemetryAuthMode;

pub use crate::auth::storage::AuthCredentialsStoreMode;
pub use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthStorageBackend;
pub use crate::auth::storage::ProviderAuthRecord;
use crate::auth::storage::create_auth_storage;
use crate::error::RefreshTokenFailedError;
use crate::error::RefreshTokenFailedReason;
use crate::token_data::KnownPlan as InternalKnownPlan;
use crate::token_data::PlanType as InternalPlanType;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use chaos_ipc::account::PlanType as AccountPlanType;
use codex_client::ChaosHttpClient;
use thiserror::Error;

// Re-export the public surface from submodules.
pub use permissions::disconnect_all_provider_accounts;
pub use permissions::disconnect_provider_account;
pub use permissions::enforce_login_restrictions;
pub use permissions::login_with_api_key;
pub use permissions::login_with_chatgpt_auth_tokens;
pub use permissions::login_with_provider_api_key;
pub use policies::UnauthorizedRecovery;
pub use policies::UnauthorizedRecoveryStepResult;
pub use tokens::AuthManager;
pub use tokens::CLIENT_ID;

pub const DEFAULT_AUTH_PROVIDER_ID: &str = "openai";

/// Account type for the current user.
///
/// This is used internally to determine the base URL for generating responses,
/// and to gate ChatGPT-only behaviors like rate limits and available models (as
/// opposed to API key-based auth).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    ApiKey,
    Chatgpt,
}

impl From<AuthMode> for TelemetryAuthMode {
    fn from(mode: AuthMode) -> Self {
        match mode {
            AuthMode::ApiKey => TelemetryAuthMode::ApiKey,
            AuthMode::Chatgpt => TelemetryAuthMode::Chatgpt,
        }
    }
}

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone)]
pub enum ChaosAuth {
    ApiKey(ApiKeyAuth),
    Chatgpt(ChatgptAuth),
    ChatgptAuthTokens(ChatgptAuthTokens),
}

#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    provider_id: String,
    api_key: String,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuth {
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuthTokens {
    state: ChatgptAuthState,
}

#[derive(Debug, Clone)]
struct ChatgptAuthState {
    provider_id: String,
    auth_dot_json: Arc<Mutex<Option<AuthDotJson>>>,
    client: ChaosHttpClient,
}

impl PartialEq for ChaosAuth {
    fn eq(&self, other: &Self) -> bool {
        self.api_auth_mode() == other.api_auth_mode()
    }
}

pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CHAOS_REFRESH_TOKEN_URL_OVERRIDE";

#[derive(Debug, Error)]
pub enum RefreshTokenError {
    #[error("{0}")]
    Permanent(#[from] RefreshTokenFailedError),
    #[error(transparent)]
    Transient(#[from] std::io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthTokens {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
    pub previous_account_id: Option<String>,
}

pub trait ExternalAuthRefresher: Send + Sync {
    fn refresh(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<ExternalAuthTokens>> + Send + '_>>;
}

impl RefreshTokenError {
    pub fn failed_reason(&self) -> Option<RefreshTokenFailedReason> {
        match self {
            Self::Permanent(error) => Some(error.reason),
            Self::Transient(_) => None,
        }
    }
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
        }
    }
}

impl ChaosAuth {
    fn from_provider_record(
        provider_id: &str,
        chaos_home: &Path,
        auth_dot_json: AuthDotJson,
        record: ProviderAuthRecord,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        client: ChaosHttpClient,
    ) -> std::io::Result<Self> {
        let auth_mode = record.resolved_mode();
        if auth_mode == ApiAuthMode::ApiKey {
            let Some(api_key) = record.api_key.as_deref() else {
                return Err(std::io::Error::other("API key auth is missing a key."));
            };
            return Ok(ChaosAuth::from_api_key_with_provider_and_client(
                provider_id,
                api_key,
                client,
            ));
        }

        let storage_mode = record.storage_mode(auth_credentials_store_mode);
        let state = ChatgptAuthState {
            provider_id: provider_id.to_string(),
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };

        match auth_mode {
            ApiAuthMode::Chatgpt => {
                let storage = create_auth_storage(chaos_home.to_path_buf(), storage_mode);
                Ok(Self::Chatgpt(ChatgptAuth { state, storage }))
            }
            ApiAuthMode::ChatgptAuthTokens => {
                Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state }))
            }
            ApiAuthMode::ApiKey => unreachable!("api key mode is handled above"),
        }
    }

    /// Loads the available auth information from auth storage.
    pub fn from_auth_storage(
        chaos_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> std::io::Result<Option<Self>> {
        load_auth(
            chaos_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
        )
    }

    pub fn auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => AuthMode::Chatgpt,
        }
    }

    pub fn api_auth_mode(&self) -> ApiAuthMode {
        match self {
            Self::ApiKey(_) => ApiAuthMode::ApiKey,
            Self::Chatgpt(_) => ApiAuthMode::Chatgpt,
            Self::ChatgptAuthTokens(_) => ApiAuthMode::ChatgptAuthTokens,
        }
    }

    pub fn is_api_key_auth(&self) -> bool {
        self.auth_mode() == AuthMode::ApiKey
    }

    pub fn is_chatgpt_auth(&self) -> bool {
        self.auth_mode() == AuthMode::Chatgpt
    }

    pub fn is_external_chatgpt_tokens(&self) -> bool {
        matches!(self, Self::ChatgptAuthTokens(_))
    }

    /// Returns `None` if `auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => None,
        }
    }

    /// Returns `Err` if `is_chatgpt_auth()` is false.
    pub fn get_token_data(&self) -> Result<TokenData, std::io::Error> {
        self.get_current_token_data()
            .ok_or(std::io::Error::other("Token data is not available."))
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => {
                let access_token = self.get_token_data()?.access_token;
                Ok(access_token)
            }
        }
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_account_id(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.account_id)
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_account_email(&self) -> Option<String> {
        self.get_current_token_data().and_then(|t| t.id_token.email)
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub fn get_chatgpt_user_id(&self) -> Option<String> {
        self.get_current_token_data()
            .and_then(|t| t.id_token.chatgpt_user_id)
    }

    /// Account-facing plan classification derived from the current token.
    pub fn account_plan_type(&self) -> Option<AccountPlanType> {
        let map_known = |kp: &InternalKnownPlan| match kp {
            InternalKnownPlan::Free => AccountPlanType::Free,
            InternalKnownPlan::Go => AccountPlanType::Go,
            InternalKnownPlan::Plus => AccountPlanType::Plus,
            InternalKnownPlan::Pro => AccountPlanType::Pro,
            InternalKnownPlan::Team => AccountPlanType::Team,
            InternalKnownPlan::Business => AccountPlanType::Business,
            InternalKnownPlan::Enterprise => AccountPlanType::Enterprise,
            InternalKnownPlan::Edu => AccountPlanType::Edu,
        };

        self.get_current_token_data().map(|t| {
            t.id_token
                .chatgpt_plan_type
                .map(|pt| match pt {
                    InternalPlanType::Known(k) => map_known(&k),
                    InternalPlanType::Unknown(_) => AccountPlanType::Unknown,
                })
                .unwrap_or(AccountPlanType::Unknown)
        })
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    pub(crate) fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        let state = match self {
            Self::Chatgpt(auth) => &auth.state,
            Self::ChatgptAuthTokens(auth) => &auth.state,
            Self::ApiKey(_) => return None,
        };
        #[expect(clippy::unwrap_used)]
        state.auth_dot_json.lock().unwrap().clone()
    }

    /// Returns `None` if `is_chatgpt_auth()` is false.
    fn get_current_token_data(&self) -> Option<TokenData> {
        let provider_id = match self {
            Self::Chatgpt(auth) => auth.state.provider_id.as_str(),
            Self::ChatgptAuthTokens(auth) => auth.state.provider_id.as_str(),
            Self::ApiKey(_) => return None,
        };
        self.get_current_auth_json()
            .and_then(|t| t.provider_record(provider_id))
            .and_then(|record| record.tokens)
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let mut auth_dot_json = AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            providers: Default::default(),
        };
        auth_dot_json.set_provider_record(
            DEFAULT_AUTH_PROVIDER_ID,
            ProviderAuthRecord {
                auth_mode: Some(ApiAuthMode::Chatgpt),
                api_key: None,
                tokens: Some(TokenData {
                    id_token: Default::default(),
                    access_token: "Access Token".to_string(),
                    refresh_token: "test".to_string(),
                    account_id: Some("account_id".to_string()),
                }),
                last_refresh: Some(Timestamp::now()),
            },
        );

        let client = crate::default_client::create_client();
        let state = ChatgptAuthState {
            provider_id: DEFAULT_AUTH_PROVIDER_ID.to_string(),
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };
        let storage = create_auth_storage(PathBuf::new(), AuthCredentialsStoreMode::File);
        Self::Chatgpt(ChatgptAuth { state, storage })
    }

    fn from_api_key_with_provider_and_client(
        provider_id: &str,
        api_key: &str,
        _client: ChaosHttpClient,
    ) -> Self {
        Self::ApiKey(ApiKeyAuth {
            provider_id: provider_id.to_string(),
            api_key: api_key.to_owned(),
        })
    }

    fn from_api_key_with_client(api_key: &str, client: ChaosHttpClient) -> Self {
        Self::from_api_key_with_provider_and_client(DEFAULT_AUTH_PROVIDER_ID, api_key, client)
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::from_api_key_with_client(api_key, crate::default_client::create_client())
    }

    pub fn provider_id(&self) -> &str {
        match self {
            Self::ApiKey(auth) => auth.provider_id.as_str(),
            Self::Chatgpt(auth) => auth.state.provider_id.as_str(),
            Self::ChatgptAuthTokens(auth) => auth.state.provider_id.as_str(),
        }
    }
}

impl ChatgptAuth {
    pub(crate) fn current_auth_json(&self) -> Option<AuthDotJson> {
        #[expect(clippy::unwrap_used)]
        self.state.auth_dot_json.lock().unwrap().clone()
    }

    pub(crate) fn current_token_data(&self) -> Option<TokenData> {
        self.current_auth_json()
            .and_then(|auth| auth.provider_record(&self.state.provider_id))
            .and_then(|record| record.tokens)
    }

    pub(crate) fn storage(&self) -> &Arc<dyn AuthStorageBackend> {
        &self.storage
    }

    pub(crate) fn client(&self) -> &ChaosHttpClient {
        &self.state.client
    }

    pub(crate) fn provider_id(&self) -> &str {
        &self.state.provider_id
    }
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CHAOS_API_KEY_ENV_VAR: &str = "CHAOS_API_KEY";

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_chaos_api_key_from_env() -> Option<String> {
    env::var(CHAOS_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Delete the auth.json file inside `chaos_home` if it exists. Returns `Ok(true)`
/// if a file was removed, `Ok(false)` if no auth file was present.
pub fn logout(
    chaos_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    let storage = create_auth_storage(chaos_home.to_path_buf(), auth_credentials_store_mode);
    storage.delete()
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    chaos_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let storage = create_auth_storage(chaos_home.to_path_buf(), auth_credentials_store_mode);
    storage.save(auth)
}

/// Load CLI auth data using the configured credential store backend.
/// Returns `None` when no credentials are stored. This function is
/// provided only for tests.
pub fn load_auth_dot_json(
    chaos_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<AuthDotJson>> {
    let storage = create_auth_storage(chaos_home.to_path_buf(), auth_credentials_store_mode);
    storage.load()
}

impl AuthDotJson {
    pub(crate) fn from_external_tokens(external: &ExternalAuthTokens) -> std::io::Result<Self> {
        let mut token_info =
            parse_chatgpt_jwt_claims(&external.access_token).map_err(std::io::Error::other)?;
        token_info.chatgpt_account_id = Some(external.chatgpt_account_id.clone());
        token_info.chatgpt_plan_type = external
            .chatgpt_plan_type
            .as_deref()
            .map(InternalPlanType::from_raw_value)
            .or(token_info.chatgpt_plan_type)
            .or(Some(InternalPlanType::Unknown("unknown".to_string())));
        let tokens = TokenData {
            id_token: token_info,
            access_token: external.access_token.clone(),
            refresh_token: String::new(),
            account_id: Some(external.chatgpt_account_id.clone()),
        };
        let mut auth = Self {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            providers: Default::default(),
        };
        auth.set_provider_record(
            DEFAULT_AUTH_PROVIDER_ID,
            ProviderAuthRecord {
                auth_mode: Some(ApiAuthMode::ChatgptAuthTokens),
                api_key: None,
                tokens: Some(tokens),
                last_refresh: Some(Timestamp::now()),
            },
        );

        Ok(auth)
    }

    pub(crate) fn from_external_access_token(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> std::io::Result<Self> {
        let external = ExternalAuthTokens {
            access_token: access_token.to_string(),
            chatgpt_account_id: chatgpt_account_id.to_string(),
            chatgpt_plan_type: chatgpt_plan_type.map(str::to_string),
        };
        Self::from_external_tokens(&external)
    }
}

pub(crate) fn load_auth(
    chaos_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<ChaosAuth>> {
    load_auth_for_provider(
        chaos_home,
        DEFAULT_AUTH_PROVIDER_ID,
        enable_codex_api_key_env,
        auth_credentials_store_mode,
    )
}

pub(crate) fn load_auth_for_provider(
    chaos_home: &Path,
    provider_id: &str,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<ChaosAuth>> {
    let build_auth = |auth_dot_json: AuthDotJson, storage_mode| {
        let record = auth_dot_json.provider_record(provider_id)?;
        let client = crate::default_client::create_client();
        ChaosAuth::from_provider_record(
            provider_id,
            chaos_home,
            auth_dot_json,
            record,
            storage_mode,
            client,
        )
        .ok()
    };

    // API key via env var takes precedence over any other auth method.
    if provider_id == DEFAULT_AUTH_PROVIDER_ID
        && enable_codex_api_key_env
        && let Some(api_key) = read_chaos_api_key_from_env()
    {
        let client = crate::default_client::create_client();
        return Ok(Some(ChaosAuth::from_api_key_with_client(
            api_key.as_str(),
            client,
        )));
    }

    // External ChatGPT auth tokens live in the in-memory (ephemeral) store.
    let ephemeral_storage = create_auth_storage(
        chaos_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
    );
    if let Some(auth_dot_json) = ephemeral_storage.load()?
        && let Some(auth) = build_auth(auth_dot_json, AuthCredentialsStoreMode::Ephemeral)
    {
        return Ok(Some(auth));
    }

    // If the caller explicitly requested ephemeral auth, there is no persisted fallback.
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }

    // Fall back to the configured persistent store (file/keyring/auto) for managed auth.
    let storage = create_auth_storage(chaos_home.to_path_buf(), auth_credentials_store_mode);
    let auth_dot_json = match storage.load()? {
        Some(auth) => auth,
        None => return Ok(None),
    };

    Ok(build_auth(auth_dot_json, auth_credentials_store_mode))
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
