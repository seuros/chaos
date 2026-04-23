use base64::Engine;
use chaos_abi::AbiError;
use chaos_parrot::AuthProvider as ApiAuthProvider;
use chaos_parrot::TransportError;
use chaos_parrot::error::ApiError;
use chaos_parrot::rate_limits::parse_rate_limit_for_limit;
use http::HeaderMap;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::auth::ChaosAuth;
use crate::error::ChaosErr;
use crate::error::ProviderAuthMissingError;
use crate::error::RetryLimitReachedError;
use crate::error::UnexpectedResponseError;
use crate::error::UsageLimitReachedError;
use crate::model_provider_info::ANTHROPIC_PROVIDER_ID;
use crate::model_provider_info::ModelProviderInfo;
use crate::model_provider_info::OPENAI_PROVIDER_ID;
use crate::model_provider_info::is_anthropic_wire;

pub(crate) fn map_api_error(err: ApiError) -> ChaosErr {
    match err {
        ApiError::ContextWindowExceeded => ChaosErr::ContextWindowExceeded,
        ApiError::QuotaExceeded => ChaosErr::QuotaExceeded,
        ApiError::UsageNotIncluded => ChaosErr::QuotaExceeded,
        ApiError::Retryable { message, delay } => ChaosErr::Stream(message, delay),
        ApiError::Stream(msg) => ChaosErr::Stream(msg, None),
        ApiError::ServerOverloaded => ChaosErr::ServerOverloaded,
        ApiError::Api { status, message } => ChaosErr::UnexpectedStatus(UnexpectedResponseError {
            status,
            body: message,
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        }),
        ApiError::InvalidRequest { message } => ChaosErr::InvalidRequest(message),
        ApiError::Transport(transport) => match transport {
            TransportError::Http {
                status,
                url,
                headers,
                body,
            } => {
                let body_text = body.unwrap_or_default();

                if status == http::StatusCode::SERVICE_UNAVAILABLE
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_text)
                    && matches!(
                        value
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(serde_json::Value::as_str),
                        Some("server_is_overloaded" | "slow_down")
                    )
                {
                    return ChaosErr::ServerOverloaded;
                }

                if status == http::StatusCode::BAD_REQUEST {
                    if body_text
                        .contains("The image data you provided does not represent a valid image")
                    {
                        ChaosErr::InvalidImageRequest()
                    } else {
                        ChaosErr::InvalidRequest(body_text)
                    }
                } else if status == http::StatusCode::INTERNAL_SERVER_ERROR {
                    ChaosErr::InternalServerError
                } else if status == http::StatusCode::TOO_MANY_REQUESTS {
                    if let Ok(err) = serde_json::from_str::<UsageErrorResponse>(&body_text) {
                        if err.error.error_type.as_deref() == Some("usage_limit_reached") {
                            let limit_id = extract_header(headers.as_ref(), ACTIVE_LIMIT_HEADER);
                            let rate_limits = headers.as_ref().and_then(|map| {
                                parse_rate_limit_for_limit(map, limit_id.as_deref())
                            });
                            let resets_at = err
                                .error
                                .resets_at
                                .and_then(|seconds| Timestamp::from_second(seconds).ok());
                            return ChaosErr::UsageLimitReached(UsageLimitReachedError {
                                resets_at,
                                rate_limits: rate_limits.map(Box::new),
                            });
                        } else if err.error.error_type.as_deref() == Some("usage_not_included") {
                            return ChaosErr::QuotaExceeded;
                        }
                    }

                    ChaosErr::RetryLimit(RetryLimitReachedError {
                        status,
                        request_id: extract_request_tracking_id(headers.as_ref()),
                    })
                } else {
                    ChaosErr::UnexpectedStatus(UnexpectedResponseError {
                        status,
                        body: body_text,
                        url,
                        cf_ray: extract_header(headers.as_ref(), CF_RAY_HEADER),
                        request_id: extract_request_id(headers.as_ref()),
                        identity_authorization_error: extract_header(
                            headers.as_ref(),
                            X_OPENAI_AUTHORIZATION_ERROR_HEADER,
                        ),
                        identity_error_code: extract_x_error_json_code(headers.as_ref()),
                    })
                }
            }
            TransportError::RetryLimit => ChaosErr::RetryLimit(RetryLimitReachedError {
                status: http::StatusCode::INTERNAL_SERVER_ERROR,
                request_id: None,
            }),
            TransportError::Timeout => ChaosErr::Timeout,
            TransportError::Network(msg) | TransportError::Build(msg) => {
                ChaosErr::ConnectionFailed(crate::error::ConnectionFailedError {
                    source: Box::new(std::io::Error::other(msg)),
                })
            }
        },
        ApiError::RateLimit(msg) => ChaosErr::Stream(msg, None),
    }
}

pub(crate) fn abi_error_to_api_error(err: AbiError) -> ApiError {
    match err {
        AbiError::ContextWindowExceeded => ApiError::ContextWindowExceeded,
        AbiError::QuotaExceeded => ApiError::QuotaExceeded,
        AbiError::UsageNotIncluded => ApiError::UsageNotIncluded,
        AbiError::ServerOverloaded => ApiError::ServerOverloaded,
        AbiError::InvalidRequest { message } => ApiError::InvalidRequest { message },
        AbiError::Stream(message) => ApiError::Stream(message),
        AbiError::Transport { status: 0, message } => {
            ApiError::Transport(TransportError::Network(message))
        }
        AbiError::Transport { status, message } => ApiError::Transport(TransportError::Http {
            status: http::StatusCode::from_u16(status)
                .unwrap_or(http::StatusCode::INTERNAL_SERVER_ERROR),
            url: None,
            headers: None,
            body: Some(message),
        }),
        AbiError::Retryable { message, delay } => ApiError::Retryable { message, delay },
    }
}

const ACTIVE_LIMIT_HEADER: &str = "x-chaos-active-limit";
const REQUEST_ID_HEADER: &str = "x-request-id";
const OAI_REQUEST_ID_HEADER: &str = "x-oai-request-id";
const CF_RAY_HEADER: &str = "cf-ray";
const X_OPENAI_AUTHORIZATION_ERROR_HEADER: &str = "x-openai-authorization-error";
const X_ERROR_JSON_HEADER: &str = "x-error-json";

#[cfg(test)]
#[path = "api_bridge_tests.rs"]
mod tests;

fn extract_request_tracking_id(headers: Option<&HeaderMap>) -> Option<String> {
    extract_request_id(headers).or_else(|| extract_header(headers, CF_RAY_HEADER))
}

fn extract_request_id(headers: Option<&HeaderMap>) -> Option<String> {
    extract_header(headers, REQUEST_ID_HEADER)
        .or_else(|| extract_header(headers, OAI_REQUEST_ID_HEADER))
}

fn extract_header(headers: Option<&HeaderMap>, name: &str) -> Option<String> {
    headers.and_then(|map| {
        map.get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    })
}

fn extract_x_error_json_code(headers: Option<&HeaderMap>) -> Option<String> {
    let encoded = extract_header(headers, X_ERROR_JSON_HEADER)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let parsed = serde_json::from_slice::<Value>(&decoded).ok()?;
    parsed
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(crate) fn auth_provider_from_auth(
    auth: Option<ChaosAuth>,
    provider: &ModelProviderInfo,
) -> crate::error::Result<CoreAuthProvider> {
    if let Some(auth) = auth.as_ref()
        && auth.is_api_key_auth()
    {
        return Ok(CoreAuthProvider {
            token: Some(auth.get_token()?),
            account_id: None,
        });
    }

    match provider.api_key() {
        Ok(Some(api_key)) => {
            return Ok(CoreAuthProvider {
                token: Some(api_key),
                account_id: None,
            });
        }
        Ok(None) => {}
        Err(ChaosErr::EnvVar(_)) => {
            if auth.is_none() {
                return Err(provider_auth_missing(provider));
            }
        }
        Err(other) => return Err(other),
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: None,
        });
    }

    if let Some(auth) = auth {
        let token = auth.get_token()?;
        return Ok(CoreAuthProvider {
            token: Some(token),
            account_id: auth.get_account_id(),
        });
    }

    // No cached login, no bearer, and no env key fallback. A provider that
    // requires OpenAI-style auth must not send an unauthenticated request —
    // stop the turn here with a vendor-agnostic error so the client can
    // prompt for credentials instead of looping on silent 401s.
    if provider.requires_managed_auth() || provider.env_key.is_some() {
        return Err(provider_auth_missing(provider));
    }

    // Self-hosted providers (Ollama, TensorZero) don't need credentials.
    Ok(CoreAuthProvider {
        token: None,
        account_id: None,
    })
}

pub(crate) fn provider_auth_missing(provider: &ModelProviderInfo) -> ChaosErr {
    ChaosErr::ProviderAuthMissing(ProviderAuthMissingError {
        provider_id: stable_provider_id(provider),
        provider_name: provider.name.clone(),
        env_key: provider.env_key.clone(),
        env_key_instructions: provider.env_key_instructions.clone(),
        supports_oauth: provider.supports_chatgpt_account_auth(),
    })
}

/// Best-effort stable id for the provider. Built-ins are pinned to their
/// registry constants; everyone else gets the raw display `name` so the
/// client can match without guessing at an invisible kebab-case transform.
fn stable_provider_id(provider: &ModelProviderInfo) -> String {
    if provider.is_openai() {
        return OPENAI_PROVIDER_ID.to_string();
    }
    if is_anthropic_wire(provider.base_url.as_deref()) {
        return ANTHROPIC_PROVIDER_ID.to_string();
    }
    provider.name.clone()
}

#[derive(Debug, Deserialize)]
struct UsageErrorResponse {
    error: UsageErrorBody,
}

#[derive(Debug, Deserialize)]
struct UsageErrorBody {
    #[serde(rename = "type")]
    error_type: Option<String>,
    resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct CoreAuthProvider {
    token: Option<String>,
    account_id: Option<String>,
}

impl CoreAuthProvider {
    pub(crate) fn auth_header_attached(&self) -> bool {
        self.token
            .as_ref()
            .is_some_and(|token| http::HeaderValue::from_str(&format!("Bearer {token}")).is_ok())
    }

    pub(crate) fn auth_header_name(&self) -> Option<&'static str> {
        self.auth_header_attached().then_some("authorization")
    }

    #[cfg(test)]
    pub(crate) fn for_test(token: Option<&str>, account_id: Option<&str>) -> Self {
        Self {
            token: token.map(str::to_string),
            account_id: account_id.map(str::to_string),
        }
    }
}

impl ApiAuthProvider for CoreAuthProvider {
    fn bearer_token(&self) -> Option<String> {
        self.token.clone()
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}
