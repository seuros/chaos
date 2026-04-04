//! Trait implementations for core types — bridges between `chaos-traits` abstractions and
//! the concrete `Config`, `Session`, and service types defined in chaos-kern.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ServiceTier;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::Event;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_proc::StateRuntime;
use chaos_sysctl::Constrained;
use chaos_sysctl::features::Features;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::MemoriesConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use chaos_syslog::SessionTelemetry;
use chaos_traits::AgentSpawnConfig;
use chaos_traits::ConciergeConfig;
use chaos_traits::EventEmitter;
use chaos_traits::MementoConfig;
use chaos_traits::RolloutConfig;
use chaos_traits::StateAccess;
use chaos_traits::TelemetrySource;

use crate::chaos::Session;
use crate::config::Config;

// ---------------------------------------------------------------------------
// Config trait impls
// ---------------------------------------------------------------------------

impl RolloutConfig for Config {
    fn chaos_home(&self) -> &Path {
        &self.chaos_home
    }

    fn sqlite_home(&self) -> &Path {
        &self.sqlite_home
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn model_provider_id(&self) -> &str {
        &self.model_provider_id
    }

    fn generate_memories(&self) -> bool {
        self.memories.generate_memories
    }
}

impl MementoConfig for Config {
    fn chaos_home(&self) -> &Path {
        &self.chaos_home
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn ephemeral(&self) -> bool {
        self.ephemeral
    }

    fn memories(&self) -> &MemoriesConfig {
        &self.memories
    }

    fn features(&self) -> &Features {
        self.features.get()
    }

    fn approval_policy(&self) -> &Constrained<ApprovalPolicy> {
        &self.permissions.approval_policy
    }

    fn sandbox_policy(&self) -> &Constrained<SandboxPolicy> {
        &self.permissions.sandbox_policy
    }

    fn service_tier(&self) -> Option<ServiceTier> {
        self.service_tier
    }
}

impl ConciergeConfig for Config {
    fn chaos_home(&self) -> &Path {
        &self.chaos_home
    }

    fn mcp_servers(&self) -> &Constrained<HashMap<String, McpServerConfig>> {
        &self.mcp_servers
    }

    fn mcp_oauth_credentials_store_mode(&self) -> OAuthCredentialsStoreMode {
        self.mcp_oauth_credentials_store_mode
    }

    fn mcp_oauth_callback_port(&self) -> Option<u16> {
        self.mcp_oauth_callback_port
    }

    fn mcp_oauth_callback_url(&self) -> Option<&str> {
        self.mcp_oauth_callback_url.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Session trait impls
// ---------------------------------------------------------------------------

impl EventEmitter for Session {
    fn event_sender(&self) -> async_channel::Sender<Event> {
        self.tx_event.clone()
    }
}

impl StateAccess for Session {
    fn state_db(&self) -> Option<Arc<StateRuntime>> {
        self.services.state_db.clone()
    }
}

impl TelemetrySource for Session {
    fn session_telemetry(&self) -> &SessionTelemetry {
        &self.services.session_telemetry
    }
}

impl chaos_traits::AgentSpawner for Session {
    fn conversation_id(&self) -> ProcessId {
        self.conversation_id
    }

    async fn spawn_agent(
        &self,
        _config: AgentSpawnConfig,
        _prompt: String,
    ) -> anyhow::Result<ProcessId> {
        // TODO: Wire to self.services.agent_control.spawn_agent() during Phase D migration.
        // The full implementation requires building an AgentConfig from AgentSpawnConfig,
        // which depends on types not yet extracted. This stub compiles and will be completed
        // when chaos-memento actually consumes it.
        anyhow::bail!("AgentSpawner::spawn_agent not yet wired — complete during Phase D")
    }
}
