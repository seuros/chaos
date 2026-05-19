//! Narrow config view traits — each satellite crate sees only the fields it needs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_sysctl::Constrained;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::MemoriesConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;

/// Forward each `&self -> ReturnType` accessor on a config trait through `Arc<T>`
/// to its inner `T` implementation, eliminating per-trait delegation boilerplate.
macro_rules! impl_config_arc_forward {
    ($trait:ident { $( fn $name:ident(&self) -> $ret:ty );* $(;)? }) => {
        impl<T: $trait> $trait for Arc<T> {
            $(
                fn $name(&self) -> $ret {
                    (**self).$name()
                }
            )*
        }
    };
}

/// Minimal config surface for rollout persistence (recorder, metadata, list).
pub trait RolloutConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn sqlite_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn model_provider_id(&self) -> &str;
    fn generate_memories(&self) -> bool;
}

impl_config_arc_forward!(RolloutConfig {
    fn chaos_home(&self) -> &Path;
    fn sqlite_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn model_provider_id(&self) -> &str;
    fn generate_memories(&self) -> bool;
});

/// Config surface for the memory subsystem (phase1, phase2, start).
pub trait MementoConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn ephemeral(&self) -> bool;
    fn memories(&self) -> &MemoriesConfig;
    fn approval_policy(&self) -> &Constrained<ApprovalPolicy>;
    fn sandbox_policy(&self) -> &Constrained<SandboxPolicy>;
    fn service_tier(&self) -> Option<ServiceTier>;
}

impl_config_arc_forward!(MementoConfig {
    fn chaos_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn ephemeral(&self) -> bool;
    fn memories(&self) -> &MemoriesConfig;
    fn approval_policy(&self) -> &Constrained<ApprovalPolicy>;
    fn sandbox_policy(&self) -> &Constrained<SandboxPolicy>;
    fn service_tier(&self) -> Option<ServiceTier>;
});

/// Config surface for MCP connection management (concierge).
pub trait ConciergeConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn mcp_servers(&self) -> &Constrained<HashMap<String, McpServerConfig>>;
    fn mcp_oauth_credentials_store_mode(&self) -> OAuthCredentialsStoreMode;
    fn mcp_oauth_callback_port(&self) -> Option<u16>;
    fn mcp_oauth_callback_url(&self) -> Option<&str>;
}

impl_config_arc_forward!(ConciergeConfig {
    fn chaos_home(&self) -> &Path;
    fn mcp_servers(&self) -> &Constrained<HashMap<String, McpServerConfig>>;
    fn mcp_oauth_credentials_store_mode(&self) -> OAuthCredentialsStoreMode;
    fn mcp_oauth_callback_port(&self) -> Option<u16>;
    fn mcp_oauth_callback_url(&self) -> Option<&str>;
});
