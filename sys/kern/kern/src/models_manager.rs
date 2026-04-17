pub mod manager;
pub mod model_info;

// Re-export from chaos-model-catalog so existing crate-internal paths keep working.
pub use chaos_model_catalog::ModelDiscoveryWorkflow;
pub use chaos_model_catalog::ModelsCache;
pub use chaos_model_catalog::ModelsCacheManager;
pub use chaos_model_catalog::ModelsCacheScope;
pub use chaos_model_catalog::RefreshStrategy;

// Collaboration-mode config re-exported for external consumers (e.g. test crates).
pub use crate::collaboration_modes::CollaborationModesConfig;

/// Convert the client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3").
pub fn client_version_to_whole() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    )
}
