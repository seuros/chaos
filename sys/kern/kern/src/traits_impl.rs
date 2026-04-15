//! Trait implementations for core types — bridges between `chaos-traits` abstractions and
//! the concrete `Config`, `Session`, and service types defined in chaos-kern.

use chaos_ipc::ProcessId;
use chaos_ipc::protocol::Event;
use chaos_sysctl::Constrained;
use chaos_sysctl::types::McpServerConfig;
use chaos_sysctl::types::OAuthCredentialsStoreMode;
use chaos_syslog::SessionTelemetry;
use chaos_traits::AgentSpawnConfig;
use chaos_traits::ConciergeConfig;
use chaos_traits::EventEmitter;
use chaos_traits::RolloutConfig;
use chaos_traits::RuntimeAccess;
use chaos_traits::TelemetrySource;
use std::collections::HashMap;
use std::path::Path;

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
        // Memory subsystem evicted — always disabled.
        false
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

impl RuntimeAccess for Session {
    fn has_runtime_db(&self) -> bool {
        self.services.runtime_db.is_some()
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
        config: AgentSpawnConfig,
        prompt: String,
    ) -> anyhow::Result<ProcessId> {
        // Clone the session's base Config and overlay the caller's
        // model / base-instructions / cwd. The satellite surface
        // (`AgentSpawnConfig`) intentionally exposes only the knobs
        // a sub-agent caller controls; every other field inherits
        // the parent session's configuration.
        let mut kern_config = self.base_config().await;
        kern_config.model = Some(config.model);
        kern_config.base_instructions = Some(config.instructions);
        kern_config.cwd = config.cwd;

        let parent_source = self.session_source().await;
        let depth = crate::minions::next_process_spawn_depth(&parent_source);
        let session_source = chaos_ipc::protocol::SessionSource::SubAgent(
            chaos_ipc::protocol::SubAgentSource::ProcessSpawn {
                parent_process_id: self.conversation_id,
                depth,
                agent_nickname: None,
                agent_role: None,
            },
        );

        let items = vec![chaos_ipc::user_input::UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }];

        let conversation_id = self.conversation_id;
        self.services
            .agent_control
            .spawn_agent(kern_config, items, Some(session_source))
            .await
            .map_err(|err| {
                anyhow::anyhow!(
                    "AgentSpawner::spawn_agent failed (parent conversation_id={conversation_id}): {err}"
                )
            })
    }
}
