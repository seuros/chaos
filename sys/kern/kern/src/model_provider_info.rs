//! Registry of model providers supported by Chaos.
//!
//! Providers can be defined in two places:
//!   1. Built-in defaults compiled into the binary so Chaos works out-of-the-box.
//!   2. User-defined entries inside `~/.chaos/config.toml` under the `model_providers`
//!      key. These override or extend the defaults at runtime.

use crate::auth::AuthMode;
use crate::error::EnvVarError;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_parrot::Provider as ApiProvider;
use chaos_parrot::provider::RetryConfig as ApiRetryConfig;
use http::HeaderMap;
use http::header::HeaderName;
use http::header::HeaderValue;
use http::header::USER_AGENT;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

const DEFAULT_STREAM_IDLE_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_STREAM_MAX_RETRIES: u64 = 5;
const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
/// Hard cap for user-configured `stream_max_retries`.
const MAX_STREAM_MAX_RETRIES: u64 = 100;
/// Hard cap for user-configured `request_max_retries`.
const MAX_REQUEST_MAX_RETRIES: u64 = 100;

const OPENAI_PROVIDER_NAME: &str = "OpenAI";
pub const OPENAI_PROVIDER_ID: &str = "openai";
pub const OPENAI_DEFAULT_BASE_URL: &str = chaos_services::openai::OPENAI_API_BASE;
const CHATGPT_DEFAULT_BASE_URL: &str = chaos_services::openai::CHATGPT_BACKEND_BASE;

const ANTHROPIC_PROVIDER_NAME: &str = "Anthropic";
pub const ANTHROPIC_PROVIDER_ID: &str = "anthropic";
pub const ANTHROPIC_DEFAULT_BASE_URL: &str = chaos_services::anthropic::API_BASE;

/// Wire protocol that the provider speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    /// Lazy auto-detection: try Responses first, fall back to Chat Completions
    /// on 404/405/501. The winning format is cached for the session.
    #[default]
    Auto,
    /// The Responses API exposed by OpenAI at `/v1/responses`.
    Responses,
    /// The Chat Completions API at `/v1/chat/completions`.
    #[serde(rename = "chat_completions")]
    ChatCompletions,
    /// TensorZero native inference API at `/inference`.
    #[serde(rename = "tensorzero")]
    TensorZero,
}

impl fmt::Display for WireApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Auto => "auto",
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
            Self::TensorZero => "tensorzero",
        };
        f.write_str(value)
    }
}

impl<'de> Deserialize<'de> for WireApi {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "auto" => Ok(Self::Auto),
            "responses" => Ok(Self::Responses),
            "chat_completions" => Ok(Self::ChatCompletions),
            "tensorzero" => Ok(Self::TensorZero),
            _ => Err(serde::de::Error::unknown_variant(
                &value,
                &["auto", "responses", "chat_completions", "tensorzero"],
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthMethod {
    ApiKey,
    ChatgptAccount,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProviderAuthCapabilities {
    #[serde(default)]
    pub methods: Vec<ProviderAuthMethod>,
}

/// Returns true if the provider's base URL indicates it speaks the Anthropic
/// Messages wire format.
///
/// Catches the real `api.anthropic.com` and the clones who bolted
/// `/anthropic` onto their base URL — MiniMax (`api.minimax.io/anthropic`),
/// Kimi (`api.moonshot.ai/anthropic`), Z.ai (`api.z.ai/api/anthropic`).
/// Imitation is the sincerest form of not having your own wire format.
pub fn is_anthropic_wire(base_url: Option<&str>) -> bool {
    base_url.map(|u| u.contains("anthropic")).unwrap_or(false)
}

/// Returns the native server-side tools a provider injects based on its base URL.
///
/// xAI exposes `web_search` and `x_search` as Responses-API server-side tools.
/// These are sent as bare `{ "type": "<name>" }` entries — no function schema.
/// Other providers get an empty list (they either don't support them or fetch
/// the list dynamically from `/models`).
pub fn native_server_side_tools_for_url(base_url: Option<&str>) -> Vec<String> {
    match base_url {
        Some(url) if url.contains("x.ai") => {
            vec!["web_search".to_string(), "x_search".to_string()]
        }
        _ => vec![],
    }
}

/// Serializable representation of a provider definition.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelProviderInfo {
    /// Friendly display name.
    pub name: String,
    /// Base URL for the provider's OpenAI-compatible API.
    pub base_url: Option<String>,
    /// Environment variable that stores the user's API key for this provider.
    pub env_key: Option<String>,

    /// Optional instructions to help the user get a valid value for the
    /// variable and set it.
    pub env_key_instructions: Option<String>,

    /// Value to use with `Authorization: Bearer <token>` header. Use of this
    /// config is discouraged in favor of `env_key` for security reasons, but
    /// this may be necessary when using this programmatically.
    pub experimental_bearer_token: Option<String>,

    /// Which wire protocol this provider expects.
    #[serde(default)]
    pub wire_api: WireApi,

    /// Optional query parameters to append to the base URL.
    pub query_params: Option<HashMap<String, String>>,

    /// Additional HTTP headers to include in requests to this provider where
    /// the (key, value) pairs are the header name and value.
    pub http_headers: Option<HashMap<String, String>>,

    /// Optional HTTP headers to include in requests to this provider where the
    /// (key, value) pairs are the header name and _environment variable_ whose
    /// value should be used. If the environment variable is not set, or the
    /// value is empty, the header will not be included in the request.
    pub env_http_headers: Option<HashMap<String, String>>,

    /// Maximum number of times to retry a failed HTTP request to this provider.
    pub request_max_retries: Option<u64>,

    /// Number of times to retry reconnecting a dropped streaming response before failing.
    pub stream_max_retries: Option<u64>,

    /// Idle timeout (in milliseconds) to wait for activity on a streaming response before treating
    /// the connection as lost.
    pub stream_idle_timeout_ms: Option<u64>,

    /// Does this provider require an OpenAI API Key or ChatGPT login token? If true,
    /// user is presented with login screen on first run, and login preference and token/key
    /// are stored in auth.json. If false (which is the default), login screen is skipped,
    /// and API key (if needed) comes from the "env_key" environment variable.
    #[serde(default)]
    pub requires_openai_auth: bool,

    /// Structured auth capabilities for this provider.
    ///
    /// When unset, Chaos derives capabilities from legacy fields:
    /// - `requires_openai_auth = true` => ChatGPT account + API key
    /// - `env_key.is_some()` => API key
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ProviderAuthCapabilities>,

    /// Whether this provider supports the Responses API WebSocket transport.
    #[serde(default)]
    pub supports_websockets: bool,

    /// Server-side tools this provider handles natively. Each entry is injected
    /// into the request's `tools` array as `{ "type": "<name>" }` — no schema,
    /// executed entirely on the provider's infrastructure.
    ///
    /// Example for xAI: `["web_search", "x_search"]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_server_side_tools: Vec<String>,
}

impl ModelProviderInfo {
    pub(crate) fn effective_base_url(&self, auth_mode: Option<AuthMode>) -> String {
        // Only route to the ChatGPT backend when the provider actually uses
        // OpenAI auth.  Self-authenticated providers (env_key, bearer token)
        // must never be redirected to chatgpt.com.
        let default_base_url = if self.supports_chatgpt_account_auth()
            && matches!(auth_mode, Some(AuthMode::Chatgpt))
        {
            CHATGPT_DEFAULT_BASE_URL
        } else {
            OPENAI_DEFAULT_BASE_URL
        };
        self.base_url
            .clone()
            .unwrap_or_else(|| default_base_url.to_string())
    }

    fn build_header_map(&self) -> crate::error::Result<HeaderMap> {
        let mut headers = crate::default_client::default_headers();
        if let Ok(user_agent) =
            HeaderValue::from_str(&crate::default_client::get_chaos_user_agent())
        {
            headers.insert(USER_AGENT, user_agent);
        }
        if let Some(extra) = &self.http_headers {
            for (k, v) in extra {
                if let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                    headers.insert(name, value);
                }
            }
        }

        if let Some(env_headers) = &self.env_http_headers {
            for (header, env_var) in env_headers {
                if let Ok(val) = std::env::var(env_var)
                    && !val.trim().is_empty()
                    && let (Ok(name), Ok(value)) =
                        (HeaderName::try_from(header), HeaderValue::try_from(val))
                {
                    headers.insert(name, value);
                }
            }
        }

        Ok(headers)
    }

    pub(crate) fn to_api_provider(
        &self,
        auth_mode: Option<AuthMode>,
    ) -> crate::error::Result<ApiProvider> {
        let headers = self.build_header_map()?;
        let retry = ApiRetryConfig {
            max_attempts: self.request_max_retries(),
            base_delay: Duration::from_millis(200),
            retry_429: false,
            retry_5xx: true,
            retry_transport: true,
        };

        Ok(ApiProvider {
            name: self.name.clone(),
            base_url: self.effective_base_url(auth_mode),
            query_params: self.query_params.clone(),
            headers,
            retry,
            stream_idle_timeout: self.stream_idle_timeout(),
        })
    }

    /// If `env_key` is Some, returns the API key for this provider if present
    /// (and non-empty) in the environment. If `env_key` is required but
    /// cannot be found, returns an error.
    pub fn api_key(&self) -> crate::error::Result<Option<String>> {
        match &self.env_key {
            Some(env_key) => {
                let api_key = std::env::var(env_key)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| {
                        crate::error::ChaosErr::EnvVar(EnvVarError {
                            var: env_key.clone(),
                            instructions: self.env_key_instructions.clone(),
                        })
                    })?;
                Ok(Some(api_key))
            }
            None => Ok(None),
        }
    }

    /// Effective maximum number of request retries for this provider.
    pub fn request_max_retries(&self) -> u64 {
        self.request_max_retries
            .unwrap_or(DEFAULT_REQUEST_MAX_RETRIES)
            .min(MAX_REQUEST_MAX_RETRIES)
    }

    /// Effective maximum number of stream reconnection attempts for this provider.
    pub fn stream_max_retries(&self) -> u64 {
        self.stream_max_retries
            .unwrap_or(DEFAULT_STREAM_MAX_RETRIES)
            .min(MAX_STREAM_MAX_RETRIES)
    }

    /// Effective idle timeout for streaming responses.
    pub fn stream_idle_timeout(&self) -> Duration {
        self.stream_idle_timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(DEFAULT_STREAM_IDLE_TIMEOUT_MS))
    }

    pub fn auth_capabilities(&self) -> ProviderAuthCapabilities {
        if let Some(auth) = &self.auth {
            return auth.clone();
        }

        let mut methods = Vec::new();
        if self.requires_openai_auth {
            methods.push(ProviderAuthMethod::ChatgptAccount);
            methods.push(ProviderAuthMethod::ApiKey);
        } else if self.env_key.is_some() {
            methods.push(ProviderAuthMethod::ApiKey);
        }

        ProviderAuthCapabilities { methods }
    }

    pub fn supports_auth_method(&self, method: ProviderAuthMethod) -> bool {
        self.auth_capabilities().methods.contains(&method)
    }

    pub fn supports_chatgpt_account_auth(&self) -> bool {
        self.supports_auth_method(ProviderAuthMethod::ChatgptAccount)
    }

    pub fn supports_api_key_auth(&self) -> bool {
        self.supports_auth_method(ProviderAuthMethod::ApiKey)
    }

    pub fn requires_managed_auth(&self) -> bool {
        self.supports_chatgpt_account_auth() || self.supports_api_key_auth()
    }

    pub fn create_anthropic_provider() -> ModelProviderInfo {
        ModelProviderInfo {
            name: ANTHROPIC_PROVIDER_NAME.into(),
            base_url: Some(ANTHROPIC_DEFAULT_BASE_URL.into()),
            env_key: Some("ANTHROPIC_API_KEY".into()),
            env_key_instructions: Some(
                "Create an API key at https://console.anthropic.com/ and export \
                 it as `ANTHROPIC_API_KEY`."
                    .into(),
            ),
            experimental_bearer_token: None,
            wire_api: WireApi::Auto,
            query_params: None,
            http_headers: Some(
                [("version".to_string(), CHAOS_VERSION.to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: None,
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: false,
            auth: Some(ProviderAuthCapabilities {
                methods: vec![ProviderAuthMethod::ApiKey],
            }),
            supports_websockets: false,
            native_server_side_tools: vec![],
        }
    }

    pub fn create_openai_provider(base_url: Option<String>) -> ModelProviderInfo {
        ModelProviderInfo {
            name: OPENAI_PROVIDER_NAME.into(),
            base_url,
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: Some(
                [("version".to_string(), CHAOS_VERSION.to_string())]
                    .into_iter()
                    .collect(),
            ),
            env_http_headers: Some(
                [
                    (
                        "OpenAI-Organization".to_string(),
                        "OPENAI_ORGANIZATION".to_string(),
                    ),
                    ("OpenAI-Project".to_string(), "OPENAI_PROJECT".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            // Use global defaults for retry/timeout unless overridden in config.toml.
            request_max_retries: None,
            stream_max_retries: None,
            stream_idle_timeout_ms: None,
            requires_openai_auth: true,
            auth: Some(ProviderAuthCapabilities {
                methods: vec![
                    ProviderAuthMethod::ChatgptAccount,
                    ProviderAuthMethod::ApiKey,
                ],
            }),
            supports_websockets: true,
            native_server_side_tools: vec![],
        }
    }

    pub fn is_openai(&self) -> bool {
        self.name == OPENAI_PROVIDER_NAME
    }

    /// Returns `true` when the provider carries its own credentials — either
    /// an `env_key` or a hard-coded `experimental_bearer_token`.  In that
    /// case the session-level ChatGPT auth should not be inherited.
    pub fn is_self_authenticated(&self) -> bool {
        (self.env_key.is_some() && !self.supports_api_key_auth())
            || self.experimental_bearer_token.is_some()
    }
}

pub fn built_in_model_providers() -> HashMap<String, ModelProviderInfo> {
    use ModelProviderInfo as P;

    let mut providers: HashMap<String, ModelProviderInfo> =
        toml::from_str(chaos_services::THIRDPARTY_PROVIDERS_TOML).unwrap_or_else(|err| {
            tracing::error!(error = %err, "failed to parse bundled thirdparty.toml");
            HashMap::new()
        });

    providers.insert(
        OPENAI_PROVIDER_ID.to_string(),
        P::create_openai_provider(None),
    );
    providers.insert(
        ANTHROPIC_PROVIDER_ID.to_string(),
        P::create_anthropic_provider(),
    );

    providers
}

pub fn create_oss_provider_with_base_url(base_url: &str, wire_api: WireApi) -> ModelProviderInfo {
    ModelProviderInfo {
        name: "gpt-oss".into(),
        base_url: Some(base_url.into()),
        env_key: None,
        env_key_instructions: None,
        experimental_bearer_token: None,
        wire_api,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: None,
        stream_max_retries: None,
        stream_idle_timeout_ms: None,
        requires_openai_auth: false,
        auth: None,
        supports_websockets: false,
        native_server_side_tools: vec![],
    }
}

#[cfg(test)]
#[path = "model_provider_info_tests.rs"]
mod tests;
