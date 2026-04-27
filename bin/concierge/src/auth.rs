use std::collections::HashMap;

use anyhow::Result;
use chaos_ipc::protocol::McpAuthStatus;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::McpServerTransportConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use futures::future::join_all;
use tracing::warn;

/// Error returned by an OAuth provider (e.g. invalid_scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthProviderError {
    error: Option<String>,
    error_description: Option<String>,
}

impl OAuthProviderError {
    pub fn new(error: Option<String>, error_description: Option<String>) -> Self {
        Self {
            error,
            error_description,
        }
    }
}

impl std::fmt::Display for OAuthProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.error.as_deref(), self.error_description.as_deref()) {
            (Some(error), Some(error_description)) => {
                write!(f, "OAuth provider returned `{error}`: {error_description}")
            }
            (Some(error), None) => write!(f, "OAuth provider returned `{error}`"),
            (None, Some(error_description)) => write!(f, "OAuth error: {error_description}"),
            (None, None) => write!(f, "OAuth provider returned an error"),
        }
    }
}

impl std::error::Error for OAuthProviderError {}

#[derive(Debug, Clone)]
pub struct McpOAuthLoginConfig {
    pub url: String,
    pub http_headers: Option<HashMap<String, String>>,
    pub env_http_headers: Option<HashMap<String, String>>,
    pub discovered_scopes: Option<Vec<String>>,
}

#[derive(Debug)]
pub enum McpOAuthLoginSupport {
    Supported(McpOAuthLoginConfig),
    Unsupported,
    Unknown(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthScopesSource {
    Explicit,
    Configured,
    Discovered,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMcpOAuthScopes {
    pub scopes: Vec<String>,
    pub source: McpOAuthScopesSource,
}

pub async fn oauth_login_support(transport: &McpServerTransportConfig) -> McpOAuthLoginSupport {
    let McpServerTransportConfig::StreamableHttp {
        url,
        bearer_token,
        bearer_token_env_var,
        http_headers,
        env_http_headers,
    } = transport
    else {
        return McpOAuthLoginSupport::Unsupported;
    };

    if bearer_token.is_some() || bearer_token_env_var.is_some() {
        return McpOAuthLoginSupport::Unsupported;
    }

    // TODO(oauth): implement when mcp-guest gains OAuth support
    let _ = (url, http_headers, env_http_headers);
    McpOAuthLoginSupport::Unsupported
}

pub async fn discover_supported_scopes(
    transport: &McpServerTransportConfig,
) -> Option<Vec<String>> {
    match oauth_login_support(transport).await {
        McpOAuthLoginSupport::Supported(config) => config.discovered_scopes,
        McpOAuthLoginSupport::Unsupported | McpOAuthLoginSupport::Unknown(_) => None,
    }
}

pub fn resolve_oauth_scopes(
    explicit_scopes: Option<Vec<String>>,
    configured_scopes: Option<Vec<String>>,
    discovered_scopes: Option<Vec<String>>,
) -> ResolvedMcpOAuthScopes {
    if let Some(scopes) = explicit_scopes {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Explicit,
        };
    }

    if let Some(scopes) = configured_scopes {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Configured,
        };
    }

    if let Some(scopes) = discovered_scopes
        && !scopes.is_empty()
    {
        return ResolvedMcpOAuthScopes {
            scopes,
            source: McpOAuthScopesSource::Discovered,
        };
    }

    ResolvedMcpOAuthScopes {
        scopes: Vec::new(),
        source: McpOAuthScopesSource::Empty,
    }
}

pub fn should_retry_without_scopes(scopes: &ResolvedMcpOAuthScopes, error: &anyhow::Error) -> bool {
    scopes.source == McpOAuthScopesSource::Discovered
        && error.downcast_ref::<OAuthProviderError>().is_some()
}

#[derive(Debug, Clone)]
pub struct McpAuthStatusEntry {
    pub config: McpServerConfig,
    pub auth_status: McpAuthStatus,
}

pub async fn compute_auth_statuses<'a, I>(
    servers: I,
    store_mode: OAuthCredentialsStoreMode,
) -> HashMap<String, McpAuthStatusEntry>
where
    I: IntoIterator<Item = (&'a String, &'a McpServerConfig)>,
{
    let futures = servers.into_iter().map(|(name, config)| {
        let name = name.clone();
        let config = config.clone();
        async move {
            let auth_status = match compute_auth_status(&name, &config, store_mode).await {
                Ok(status) => status,
                Err(error) => {
                    warn!("failed to determine auth status for MCP server `{name}`: {error:?}");
                    McpAuthStatus::Unsupported
                }
            };
            let entry = McpAuthStatusEntry {
                config,
                auth_status,
            };
            (name, entry)
        }
    });

    join_all(futures).await.into_iter().collect()
}

async fn compute_auth_status(
    server_name: &str,
    config: &McpServerConfig,
    store_mode: OAuthCredentialsStoreMode,
) -> Result<McpAuthStatus> {
    // TODO(oauth): implement when mcp-guest gains OAuth support
    let _ = (server_name, store_mode);
    match &config.transport {
        McpServerTransportConfig::Stdio { .. } => Ok(McpAuthStatus::Unsupported),
        McpServerTransportConfig::StreamableHttp {
            bearer_token,
            bearer_token_env_var,
            ..
        } => {
            if bearer_token.is_some() || bearer_token_env_var.is_some() {
                Ok(McpAuthStatus::BearerToken)
            } else {
                Ok(McpAuthStatus::Unsupported)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use pretty_assertions::assert_eq;

    use super::McpOAuthScopesSource;
    use super::OAuthProviderError;
    use super::ResolvedMcpOAuthScopes;
    use super::resolve_oauth_scopes;
    use super::should_retry_without_scopes;

    #[test]
    fn resolve_oauth_scopes_prefers_explicit() {
        let resolved = resolve_oauth_scopes(
            Some(vec!["explicit".to_string()]),
            Some(vec!["configured".to_string()]),
            Some(vec!["discovered".to_string()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["explicit".to_string()],
                source: McpOAuthScopesSource::Explicit,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_prefers_configured_over_discovered() {
        let resolved = resolve_oauth_scopes(
            None,
            Some(vec!["configured".to_string()]),
            Some(vec!["discovered".to_string()]),
        );

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["configured".to_string()],
                source: McpOAuthScopesSource::Configured,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_uses_discovered_when_needed() {
        let resolved = resolve_oauth_scopes(None, None, Some(vec!["discovered".to_string()]));

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: vec!["discovered".to_string()],
                source: McpOAuthScopesSource::Discovered,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_preserves_explicitly_empty_configured_scopes() {
        let resolved = resolve_oauth_scopes(None, Some(Vec::new()), Some(vec!["ignored".into()]));

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: Vec::new(),
                source: McpOAuthScopesSource::Configured,
            }
        );
    }

    #[test]
    fn resolve_oauth_scopes_falls_back_to_empty() {
        let resolved = resolve_oauth_scopes(None, None, None);

        assert_eq!(
            resolved,
            ResolvedMcpOAuthScopes {
                scopes: Vec::new(),
                source: McpOAuthScopesSource::Empty,
            }
        );
    }

    #[test]
    fn should_retry_without_scopes_only_for_discovered_provider_errors() {
        let discovered = ResolvedMcpOAuthScopes {
            scopes: vec!["scope".to_string()],
            source: McpOAuthScopesSource::Discovered,
        };
        let provider_error = anyhow!(OAuthProviderError::new(
            Some("invalid_scope".to_string()),
            Some("scope rejected".to_string()),
        ));

        assert!(should_retry_without_scopes(&discovered, &provider_error));

        let configured = ResolvedMcpOAuthScopes {
            scopes: vec!["scope".to_string()],
            source: McpOAuthScopesSource::Configured,
        };
        assert!(!should_retry_without_scopes(&configured, &provider_error));
        assert!(!should_retry_without_scopes(
            &discovered,
            &anyhow!("timed out waiting for OAuth callback"),
        ));
    }
}
