//! Agent spawning trait — allows satellite crates to spawn sub-agents without depending on the
//! concrete AgentControl or Codex types.

use chaos_ipc::ProcessId;
use std::future::Future;

/// Configuration for spawning a sub-agent.
#[derive(Clone, Debug)]
pub struct AgentSpawnConfig {
    /// Model to use for the sub-agent.
    pub model: String,
    /// System instructions for the sub-agent.
    pub instructions: String,
    /// Working directory for the sub-agent.
    pub cwd: std::path::PathBuf,
}

/// Provides sub-agent lifecycle management.
pub trait AgentSpawner: Send + Sync {
    /// The conversation ID of the parent session.
    fn conversation_id(&self) -> ProcessId;

    /// Spawn a sub-agent with the given config and user prompt.
    /// Returns the thread ID of the spawned agent.
    fn spawn_agent(
        &self,
        config: AgentSpawnConfig,
        prompt: String,
    ) -> impl Future<Output = anyhow::Result<ProcessId>> + Send;
}
