//! Narrow config view traits — each satellite crate sees only the fields it needs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::protocol::AskForApproval;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_sysctl::Constrained;
use chaos_sysctl::features::Features;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::MemoriesConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;

/// Minimal config surface for rollout persistence (recorder, metadata, list).
pub trait RolloutConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn sqlite_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn model_provider_id(&self) -> &str;
    fn generate_memories(&self) -> bool;
}

impl<T: RolloutConfig> RolloutConfig for Arc<T> {
    fn chaos_home(&self) -> &Path {
        (**self).chaos_home()
    }
    fn sqlite_home(&self) -> &Path {
        (**self).sqlite_home()
    }
    fn cwd(&self) -> &Path {
        (**self).cwd()
    }
    fn model_provider_id(&self) -> &str {
        (**self).model_provider_id()
    }
    fn generate_memories(&self) -> bool {
        (**self).generate_memories()
    }
}

/// Config surface for the memory subsystem (phase1, phase2, start).
pub trait MementoConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn cwd(&self) -> &Path;
    fn ephemeral(&self) -> bool;
    fn memories(&self) -> &MemoriesConfig;
    fn features(&self) -> &Features;
    fn approval_policy(&self) -> &Constrained<AskForApproval>;
    fn sandbox_policy(&self) -> &Constrained<SandboxPolicy>;
    fn service_tier(&self) -> Option<ServiceTier>;
}

impl<T: MementoConfig> MementoConfig for Arc<T> {
    fn chaos_home(&self) -> &Path {
        (**self).chaos_home()
    }
    fn cwd(&self) -> &Path {
        (**self).cwd()
    }
    fn ephemeral(&self) -> bool {
        (**self).ephemeral()
    }
    fn memories(&self) -> &MemoriesConfig {
        (**self).memories()
    }
    fn features(&self) -> &Features {
        (**self).features()
    }
    fn approval_policy(&self) -> &Constrained<AskForApproval> {
        (**self).approval_policy()
    }
    fn sandbox_policy(&self) -> &Constrained<SandboxPolicy> {
        (**self).sandbox_policy()
    }
    fn service_tier(&self) -> Option<ServiceTier> {
        (**self).service_tier()
    }
}

/// Config surface for MCP connection management (concierge).
pub trait ConciergeConfig: Send + Sync {
    fn chaos_home(&self) -> &Path;
    fn mcp_servers(&self) -> &Constrained<HashMap<String, McpServerConfig>>;
    fn mcp_oauth_credentials_store_mode(&self) -> OAuthCredentialsStoreMode;
    fn mcp_oauth_callback_port(&self) -> Option<u16>;
    fn mcp_oauth_callback_url(&self) -> Option<&str>;
}

impl<T: ConciergeConfig> ConciergeConfig for Arc<T> {
    fn chaos_home(&self) -> &Path {
        (**self).chaos_home()
    }
    fn mcp_servers(&self) -> &Constrained<HashMap<String, McpServerConfig>> {
        (**self).mcp_servers()
    }
    fn mcp_oauth_credentials_store_mode(&self) -> OAuthCredentialsStoreMode {
        (**self).mcp_oauth_credentials_store_mode()
    }
    fn mcp_oauth_callback_port(&self) -> Option<u16> {
        (**self).mcp_oauth_callback_port()
    }
    fn mcp_oauth_callback_url(&self) -> Option<&str> {
        (**self).mcp_oauth_callback_url()
    }
}
