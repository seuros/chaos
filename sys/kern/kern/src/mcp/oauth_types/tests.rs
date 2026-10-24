use crate::mcp::oauth_types::OAuthProviderError;

#[test]
fn provider_error_reexport_preserves_type_identity() {
    let error = OAuthProviderError::new(Some("invalid_scope".into()), None);
    assert_eq!(error.to_string(), "OAuth provider returned `invalid_scope`");

    let error: Box<dyn std::error::Error> = Box::new(error);
    assert!(
        error
            .downcast_ref::<chaos_concierge::auth::OAuthProviderError>()
            .is_some()
    );
}
