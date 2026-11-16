//! Local API-key format checks for the harness's known providers.
//!
//! This validates the selected provider, never chooses or switches providers.
//! Rules are deliberately internal, not provider configuration. Custom provider
//! IDs receive only the common nonempty/printable-key checks.

use thiserror::Error;

/// A validation failure that never contains the supplied credential.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ApiKeyValidationError {
    #[error("API key cannot be empty.")]
    Empty,
    #[error("API key must contain only printable ASCII characters without whitespace.")]
    InvalidCharacters,
    #[error("Invalid API key for {provider}. Expected {expected}.")]
    InvalidFormat {
        provider: &'static str,
        expected: &'static str,
    },
}

/// Validate an API key before storing or using it with the selected provider.
///
/// This is a pure, offline format check, not a check of whether a credential is
/// active or has permissions. Callers that accept pasted input should trim its
/// surrounding whitespace first. Errors are safe to display or log.
pub fn validate_provider_api_key(
    provider_id: &str,
    api_key: &str,
) -> Result<(), ApiKeyValidationError> {
    if api_key.is_empty() {
        return Err(ApiKeyValidationError::Empty);
    }
    if !api_key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(ApiKeyValidationError::InvalidCharacters);
    }

    let (provider, expected, valid) = match provider_id {
        "openai" => (
            "OpenAI",
            "an OpenAI key (sk-proj-…, sk-svcacct-…, or a legacy sk-… key)",
            has_key_prefix(api_key, "sk-proj-")
                || has_key_prefix(api_key, "sk-svcacct-")
                || has_unscoped_sk_prefix(api_key),
        ),
        "anthropic" => (
            "Anthropic",
            "an Anthropic API key (sk-ant-…), not an OAuth token or an Admin API key",
            is_anthropic_api_key(api_key),
        ),
        "charm" => (
            "Charm Hyper",
            "a Hyper API key (sk-hyper-…)",
            has_key_prefix(api_key, "sk-hyper-"),
        ),
        "moonshotai-coding" => (
            "Moonshot AI Coding",
            "a Kimi Code API key (sk-kimi-…)",
            has_key_prefix(api_key, "sk-kimi-"),
        ),
        "moonshotai" => (
            "Moonshot AI",
            "a Moonshot API key (sk-…), not a coding subscription or another provider's key",
            has_unscoped_sk_prefix(api_key),
        ),
        "xai" => (
            "xAI",
            "an xAI API key (xai-…)",
            has_key_prefix(api_key, "xai-"),
        ),
        _ => return Ok(()),
    };
    if valid {
        Ok(())
    } else {
        Err(ApiKeyValidationError::InvalidFormat { provider, expected })
    }
}

fn has_key_prefix(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix)
        .is_some_and(|secret| !secret.is_empty())
}

fn has_unscoped_sk_prefix(key: &str) -> bool {
    // Legacy OpenAI and Moonshot keys share sk-. Exclude known namespaces
    // rather than guessing length/encoding rules for the opaque secret.
    has_key_prefix(key, "sk-")
        && ![
            "sk-proj-",
            "sk-svcacct-",
            "sk-ant-",
            "sk-hyper-",
            "sk-kimi-",
        ]
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

fn is_anthropic_api_key(key: &str) -> bool {
    // Anthropic issues several API-key kinds (`sk-ant-api03-`, `sk-ant-usr-`,
    // and more to come); accept any `sk-ant-<kind>-<secret>`. OAuth/setup
    // tokens belong to subscription login and admin keys only reach the Admin
    // API, so both would fail on the first request.
    key.strip_prefix("sk-ant-")
        .and_then(|rest| rest.split_once('-'))
        .is_some_and(|(kind, secret)| {
            !kind.is_empty()
                && !secret.is_empty()
                && !kind.starts_with("oat")
                && !kind.starts_with("admin")
        })
}

#[cfg(test)]
mod tests;
