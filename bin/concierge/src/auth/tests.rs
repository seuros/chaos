use anyhow::anyhow;
use pretty_assertions::assert_eq;

use super::McpOAuthScopesSource;
use super::OAuthProviderError;
use super::ResolvedMcpOAuthScopes;
use super::resolve_oauth_scopes;
use super::should_retry_without_scopes;

#[test]
fn resolve_oauth_scopes_preserves_precedence_and_empty_configuration() {
    use McpOAuthScopesSource::{Configured, Discovered, Empty, Explicit};

    let configured = Some(vec!["configured".to_string()]);
    let discovered = Some(vec!["discovered".to_string()]);
    for (label, explicit, configured, discovered, scopes, source) in [
        (
            "explicit overrides configured and discovered",
            Some(vec!["explicit".into()]),
            configured.clone(),
            discovered.clone(),
            vec!["explicit".into()],
            Explicit,
        ),
        (
            "configured overrides discovered",
            None,
            configured,
            discovered.clone(),
            vec!["configured".into()],
            Configured,
        ),
        (
            "discovered fallback",
            None,
            None,
            discovered,
            vec!["discovered".into()],
            Discovered,
        ),
        (
            "explicitly empty configuration suppresses discovery",
            None,
            Some(Vec::new()),
            Some(vec!["ignored".into()]),
            Vec::new(),
            Configured,
        ),
        ("no scopes", None, None, None, Vec::new(), Empty),
    ] {
        assert_eq!(
            resolve_oauth_scopes(explicit, configured, discovered),
            ResolvedMcpOAuthScopes { scopes, source },
            "{label}"
        );
    }
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
