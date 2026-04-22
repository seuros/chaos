use std::fmt;
use std::path::PathBuf;

use crate::ProcessId;
use crate::config_types::ApprovalsReviewer;
use crate::config_types::CollaborationMode;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::ServiceTier;
use crate::dynamic_tools::DynamicToolSpec;
use crate::models::BaseInstructions;
use crate::models::ContentItem;
use crate::models::ResponseItem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use ts_rs::TS;

use super::ApprovalPolicy;
use super::EventMsg;
use super::ReasoningEffortConfig;
use super::SandboxPolicy;
use super::SocketPolicy;
use super::VfsPolicy;

// Conversation kept for backward compatibility.
/// Response payload for `Op::GetHistory` containing the current session's
/// in-memory transcript.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ConversationPathResponseEvent {
    pub conversation_id: ProcessId,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ResumedHistory {
    pub conversation_id: ProcessId,
    pub history: Vec<RolloutItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub enum InitialHistory {
    New,
    Resumed(ResumedHistory),
    Forked(Vec<RolloutItem>),
}

impl InitialHistory {
    pub fn forked_from_id(&self) -> Option<ProcessId> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.forked_from_id,
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.id),
                _ => None,
            }),
        }
    }

    pub fn session_cwd(&self) -> Option<PathBuf> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => session_cwd_from_items(&resumed.history),
            InitialHistory::Forked(items) => session_cwd_from_items(items),
        }
    }

    pub fn get_rollout_items(&self) -> Vec<RolloutItem> {
        match self {
            InitialHistory::New => Vec::new(),
            InitialHistory::Resumed(resumed) => resumed.history.clone(),
            InitialHistory::Forked(items) => items.clone(),
        }
    }

    pub fn get_event_msgs(&self) -> Option<Vec<EventMsg>> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => Some(
                resumed
                    .history
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            InitialHistory::Forked(items) => Some(
                items
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }

    pub fn get_base_instructions(&self) -> Option<BaseInstructions> {
        // TODO: SessionMeta should (in theory) always be first in the history, so we can probably only check the first item?
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                _ => None,
            }),
        }
    }

    pub fn get_dynamic_tools(&self) -> Option<Vec<DynamicToolSpec>> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                _ => None,
            }),
        }
    }
}

fn session_cwd_from_items(items: &[RolloutItem]) -> Option<PathBuf> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.cwd.clone()),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum SessionSource {
    Cli,
    #[default]
    VSCode,
    Exec,
    Mcp,
    SubAgent(SubAgentSource),
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentSource {
    Review,
    Compact,
    ProcessSpawn {
        parent_process_id: ProcessId,
        depth: i32,
        #[serde(default)]
        agent_nickname: Option<String>,
        #[serde(default)]
        agent_role: Option<String>,
    },
    MemoryConsolidation,
    Other(String),
}

impl fmt::Display for SessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionSource::Cli => f.write_str("cli"),
            SessionSource::VSCode => f.write_str("vscode"),
            SessionSource::Exec => f.write_str("exec"),
            SessionSource::Mcp => f.write_str("mcp"),
            SessionSource::SubAgent(sub_source) => write!(f, "subagent_{sub_source}"),
            SessionSource::Unknown => f.write_str("unknown"),
        }
    }
}

impl SessionSource {
    pub fn get_nickname(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn { agent_nickname, .. }) => {
                agent_nickname.clone()
            }
            SessionSource::SubAgent(SubAgentSource::MemoryConsolidation) => {
                Some("Morpheus".to_string())
            }
            _ => None,
        }
    }

    pub fn get_agent_role(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ProcessSpawn { agent_role, .. }) => {
                agent_role.clone()
            }
            SessionSource::SubAgent(SubAgentSource::MemoryConsolidation) => {
                Some("memory builder".to_string())
            }
            _ => None,
        }
    }
}

impl fmt::Display for SubAgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubAgentSource::Review => f.write_str("review"),
            SubAgentSource::Compact => f.write_str("compact"),
            SubAgentSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            SubAgentSource::ProcessSpawn {
                parent_process_id,
                depth,
                ..
            } => {
                write!(f, "process_spawn_{parent_process_id}_d{depth}")
            }
            SubAgentSource::Other(other) => f.write_str(other),
        }
    }
}

/// SessionMeta contains session-level data that doesn't correspond to a specific turn.
///
/// NOTE: There used to be an `instructions` field here, which stored user_instructions, but we
/// now save that on TurnContext. base_instructions stores the base instructions for the session,
/// and should be used when there is no config override.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct SessionMeta {
    pub id: ProcessId,
    #[serde(
        rename = "forked_from_process_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub forked_from_id: Option<ProcessId>,
    pub timestamp: String,
    pub cwd: PathBuf,
    pub originator: String,
    pub cli_version: String,
    #[serde(default)]
    pub source: SessionSource,
    /// Optional random unique nickname assigned to an AgentControl-spawned sub-agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    pub model_provider: Option<String>,
    /// base_instructions for the session. This *should* always be present when creating a new session,
    /// but may be missing for older sessions. If not present, fall back to rendering the base_instructions
    /// from ModelsManager.
    pub base_instructions: Option<BaseInstructions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<String>,
}

impl Default for SessionMeta {
    fn default() -> Self {
        SessionMeta {
            id: ProcessId::default(),
            forked_from_id: None,
            timestamp: String::new(),
            cwd: PathBuf::new(),
            originator: String::new(),
            cli_version: String::new(),
            source: SessionSource::default(),
            agent_nickname: None,
            agent_role: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
            memory_mode: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS)]
pub struct SessionMetaLine {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    ResponseItem(ResponseItem),
    Compacted(CompactedItem),
    TurnContext(TurnContextItem),
    EventMsg(EventMsg),
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct CompactedItem {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_history: Option<Vec<ResponseItem>>,
}

impl From<CompactedItem> for ResponseItem {
    fn from(value: CompactedItem) -> Self {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: value.message,
            }],
            end_turn: None,
            phase: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct TurnContextNetworkItem {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
}

/// Persist once per real user turn after computing that turn's model-visible
/// context updates, and again after mid-turn compaction when replacement
/// history re-establishes full context, so resume/fork replay can recover the
/// latest durable baseline.
#[derive(Serialize, Clone, Debug, JsonSchema, TS)]
pub struct TurnContextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub approval_policy: ApprovalPolicy,
    pub vfs_policy: VfsPolicy,
    pub socket_policy: SocketPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<TurnContextNetworkItem>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffortConfig>,
    pub summary: ReasoningSummaryConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minion_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_output_json_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_policy: Option<TruncationPolicy>,
}

impl<'de> Deserialize<'de> for TurnContextItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct De {
            #[serde(default)]
            turn_id: Option<String>,
            #[serde(default)]
            trace_id: Option<String>,
            cwd: PathBuf,
            #[serde(default)]
            current_date: Option<String>,
            #[serde(default)]
            timezone: Option<String>,
            approval_policy: ApprovalPolicy,
            #[serde(default, alias = "file_system_sandbox_policy")]
            vfs_policy: Option<VfsPolicy>,
            #[serde(default, alias = "network_sandbox_policy")]
            socket_policy: Option<SocketPolicy>,
            #[serde(default)]
            sandbox_policy: Option<SandboxPolicy>,
            #[serde(default)]
            network: Option<TurnContextNetworkItem>,
            model: String,
            #[serde(default)]
            personality: Option<Personality>,
            #[serde(default)]
            collaboration_mode: Option<CollaborationMode>,
            #[serde(default)]
            effort: Option<ReasoningEffortConfig>,
            summary: ReasoningSummaryConfig,
            #[serde(default)]
            user_instructions: Option<String>,
            #[serde(default)]
            minion_instructions: Option<String>,
            #[serde(default)]
            final_output_json_schema: Option<Value>,
            #[serde(default)]
            truncation_policy: Option<TruncationPolicy>,
        }

        let de = De::deserialize(deserializer)?;
        let (vfs_policy, socket_policy) =
            resolve_sandbox_compat(de.vfs_policy, de.socket_policy, de.sandbox_policy.as_ref())
                .map_err(serde::de::Error::custom)?;

        Ok(TurnContextItem {
            turn_id: de.turn_id,
            trace_id: de.trace_id,
            cwd: de.cwd,
            current_date: de.current_date,
            timezone: de.timezone,
            approval_policy: de.approval_policy,
            vfs_policy,
            socket_policy,
            network: de.network,
            model: de.model,
            personality: de.personality,
            collaboration_mode: de.collaboration_mode,
            effort: de.effort,
            summary: de.summary,
            user_instructions: de.user_instructions,
            minion_instructions: de.minion_instructions,
            final_output_json_schema: de.final_output_json_schema,
            truncation_policy: de.truncation_policy,
        })
    }
}

/// Resolve the sandbox pair from either the modern (`vfs_policy` +
/// `socket_policy`) shape, the intermediate (`file_system_sandbox_policy` +
/// `network_sandbox_policy`) aliases, or the legacy single `sandbox_policy`
/// enum that predates the split.
fn resolve_sandbox_compat(
    vfs_policy: Option<VfsPolicy>,
    socket_policy: Option<SocketPolicy>,
    legacy: Option<&SandboxPolicy>,
) -> Result<(VfsPolicy, SocketPolicy), &'static str> {
    match (vfs_policy, socket_policy, legacy) {
        (Some(v), Some(s), _) => Ok((v, s)),
        (Some(v), None, Some(legacy)) => Ok((v, SocketPolicy::from(legacy))),
        (None, Some(s), Some(legacy)) => Ok((VfsPolicy::from(legacy), s)),
        (None, None, Some(legacy)) => Ok((VfsPolicy::from(legacy), SocketPolicy::from(legacy))),
        _ => Err(
            "missing sandbox policy: expected vfs_policy+socket_policy or legacy sandbox_policy",
        ),
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", content = "limit", rename_all = "snake_case")]
pub enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
pub struct RolloutLine {
    pub timestamp: String,
    #[serde(flatten)]
    pub item: RolloutItem,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct GitInfo {
    /// Current commit hash (SHA)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    /// Current branch name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Repository URL (if available from remote)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, TS)]
pub struct SessionConfiguredEvent {
    pub session_id: ProcessId,
    #[serde(
        rename = "forked_from_process_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub forked_from_id: Option<ProcessId>,

    /// Optional user-facing process name (may be unset).
    #[serde(
        rename = "process_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub process_name: Option<String>,

    /// Tell the client what model is being queried.
    pub model: String,

    pub model_provider_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,

    /// When to escalate for approval for execution
    pub approval_policy: ApprovalPolicy,

    /// Configures who approval requests are routed to for review once they have
    /// been escalated. This does not disable separate safety checks such as
    /// ARC.
    #[serde(default)]
    pub approvals_reviewer: ApprovalsReviewer,

    /// Filesystem sandbox policy applied to spawned commands.
    pub vfs_policy: VfsPolicy,

    /// Network sandbox policy applied to spawned commands.
    pub socket_policy: SocketPolicy,

    /// Working directory that should be treated as the *root* of the
    /// session.
    pub cwd: PathBuf,

    /// The effort the model is putting into reasoning about the user's request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,

    /// Identifier of the persistent message-history store (0 when unavailable).
    pub history_log_id: u64,

    /// Current number of entries in the persistent message-history store.
    pub history_entry_count: usize,

    /// Optional initial messages (as events) for resumed sessions.
    /// When present, UIs can use these to seed the history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_messages: Option<Vec<EventMsg>>,

    /// Runtime proxy bind addresses, when the managed proxy was started for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network_proxy: Option<SessionNetworkProxyRuntime>,
}

impl<'de> Deserialize<'de> for SessionConfiguredEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct De {
            session_id: ProcessId,
            #[serde(default, rename = "forked_from_process_id")]
            forked_from_id: Option<ProcessId>,
            #[serde(default, rename = "process_name")]
            process_name: Option<String>,
            model: String,
            model_provider_id: String,
            #[serde(default)]
            service_tier: Option<ServiceTier>,
            approval_policy: ApprovalPolicy,
            #[serde(default)]
            approvals_reviewer: ApprovalsReviewer,
            #[serde(default, alias = "file_system_sandbox_policy")]
            vfs_policy: Option<VfsPolicy>,
            #[serde(default, alias = "network_sandbox_policy")]
            socket_policy: Option<SocketPolicy>,
            #[serde(default)]
            sandbox_policy: Option<SandboxPolicy>,
            cwd: PathBuf,
            #[serde(default)]
            reasoning_effort: Option<ReasoningEffortConfig>,
            history_log_id: u64,
            history_entry_count: usize,
            #[serde(default)]
            initial_messages: Option<Vec<EventMsg>>,
            #[serde(default)]
            network_proxy: Option<SessionNetworkProxyRuntime>,
        }

        let de = De::deserialize(deserializer)?;
        let (vfs_policy, socket_policy) =
            resolve_sandbox_compat(de.vfs_policy, de.socket_policy, de.sandbox_policy.as_ref())
                .map_err(serde::de::Error::custom)?;

        Ok(SessionConfiguredEvent {
            session_id: de.session_id,
            forked_from_id: de.forked_from_id,
            process_name: de.process_name,
            model: de.model,
            model_provider_id: de.model_provider_id,
            service_tier: de.service_tier,
            approval_policy: de.approval_policy,
            approvals_reviewer: de.approvals_reviewer,
            vfs_policy,
            socket_policy,
            cwd: de.cwd,
            reasoning_effort: de.reasoning_effort,
            history_log_id: de.history_log_id,
            history_entry_count: de.history_entry_count,
            initial_messages: de.initial_messages,
            network_proxy: de.network_proxy,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ProcessNameUpdatedEvent {
    #[serde(rename = "process_id")]
    pub process_id: ProcessId,
    #[serde(
        rename = "process_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub process_name: Option<String>,
}
