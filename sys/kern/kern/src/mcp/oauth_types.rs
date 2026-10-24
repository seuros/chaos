//! Compatibility re-exports for kernel and CLI OAuth types.

pub use chaos_concierge::auth::OAuthProviderError;
pub use chaos_sysctl::types::OAuthCredentialsStoreMode;

#[cfg(test)]
mod tests;
