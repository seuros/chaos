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
