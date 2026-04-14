use std::collections::HashMap;

use crate::ProcessId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use super::ReasoningEffortConfig;

/// Agent lifecycle status, derived from emitted events.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Agent is waiting for initialization.
    #[default]
    PendingInit,
    /// Agent is currently running.
    Running,
    /// Agent's current turn was interrupted and it may receive more input.
    Interrupted,
    /// Agent is done. Contains the final assistant message.
    Completed(Option<String>),
    /// Agent encountered an error.
    Errored(String),
    /// Agent has been shutdown.
    Shutdown,
    /// Agent is not found.
    NotFound,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentRef {
    /// Process ID of the receiver/new agent.
    pub process_id: ProcessId,
    /// Optional nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentStatusEntry {
    /// Process ID of the receiver/new agent.
    pub process_id: ProcessId,
    /// Optional nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    /// Last known status of the agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    pub sender_process_id: ProcessId,
    /// Initial prompt sent to the agent. Can be empty to prevent CoT leaking at the
    /// beginning.
    pub prompt: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffortConfig,
    /// Kernel-selected catchphrase from the dispatched role, if any.
    /// Emitted as flavor output by the human event processor — not visible to the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catchphrase: Option<String>,
    /// Topics requested by the LLM that had no matching role.
    /// The human event processor surfaces these as a warning to the user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_topics: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the newly spawned agent, if it was created.
    #[serde(rename = "new_process_id")]
    pub new_process_id: Option<ProcessId>,
    /// Optional nickname assigned to the new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_nickname: Option<String>,
    /// Optional role assigned to the new agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_role: Option<String>,
    /// Initial prompt sent to the agent. Can be empty to prevent CoT leaking at the
    /// beginning.
    pub prompt: String,
    /// Model requested for the spawned agent.
    pub model: String,
    /// Reasoning effort requested for the spawned agent.
    pub reasoning_effort: ReasoningEffortConfig,
    /// Last known status of the new agent reported to the sender agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
    /// Prompt sent from the sender to the receiver. Can be empty to prevent CoT
    /// leaking at the beginning.
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Prompt sent from the sender to the receiver. Can be empty to prevent CoT
    /// leaking at the beginning.
    pub prompt: String,
    /// Last known status of the receiver agent reported to the sender agent.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingBeginEvent {
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process IDs of the receivers.
    #[serde(rename = "receiver_process_ids")]
    pub receiver_process_ids: Vec<ProcessId>,
    /// Optional nicknames/roles for receivers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiver_agents: Vec<CollabAgentRef>,
    /// ID of the waiting call.
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingEndEvent {
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// ID of the waiting call.
    pub call_id: String,
    /// Optional receiver metadata paired with final statuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_statuses: Vec<CollabAgentStatusEntry>,
    /// Last known status of the receiver agents reported to the sender agent.
    pub statuses: HashMap<ProcessId, AgentStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Last known status of the receiver agent reported to the sender agent before
    /// the close.
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeBeginEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeEndEvent {
    /// Identifier for the collab tool call.
    pub call_id: String,
    /// Process ID of the sender.
    #[serde(rename = "sender_process_id")]
    pub sender_process_id: ProcessId,
    /// Process ID of the receiver.
    #[serde(rename = "receiver_process_id")]
    pub receiver_process_id: ProcessId,
    /// Optional nickname assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    /// Optional role assigned to the receiver agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    /// Last known status of the receiver agent reported to the sender agent after
    /// resume.
    pub status: AgentStatus,
}
