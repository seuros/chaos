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
use crate::models::PermissionProfile;
use crate::models::ResponseItem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use super::ApprovalPolicy;
use super::EventMsg;
use super::PermissionUpdateScope;
use super::ReasoningEffortConfig;
use super::SocketPolicy;
use super::VfsPolicy;

/// Acknowledges a live permission update.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct PermissionsUpdatedEvent {
    pub scope: PermissionUpdateScope,
    pub revision: u64,
    pub approval_policy: ApprovalPolicy,
    pub vfs_policy: VfsPolicy,
    pub socket_policy: SocketPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_permissions: Option<PermissionProfile>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ResumedHistory {
    pub conversation_id: ProcessId,
    pub history: Vec<RolloutItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub enum InitialHistory {
    New,
    Resumed(ResumedHistory),
    Forked(Vec<RolloutItem>),
}

impl InitialHistory {
    /// Borrow the rollout items backing this history, or `None` for [`InitialHistory::New`].
    ///
    /// Centralizing the enum match here lets every accessor below operate on a
    /// plain slice without re-deriving which variant carries the data.
    fn rollout_items(&self) -> Option<&[RolloutItem]> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(resumed) => Some(&resumed.history),
            InitialHistory::Forked(items) => Some(items),
        }
    }

    fn find_session_meta(&self) -> Option<&SessionMeta> {
        self.rollout_items()?.iter().find_map(|item| match item {
            RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
            _ => None,
        })
    }

    pub fn forked_from_id(&self) -> Option<ProcessId> {
        match self {
            InitialHistory::New => None,
            InitialHistory::Resumed(_) => self.find_session_meta()?.forked_from_id,
            InitialHistory::Forked(_) => self.find_session_meta().map(|meta| meta.id),
        }
    }

    pub fn session_cwd(&self) -> Option<PathBuf> {
        self.find_session_meta().map(|meta| meta.cwd.clone())
    }

    pub fn get_rollout_items(&self) -> Vec<RolloutItem> {
        self.rollout_items().map(<[_]>::to_vec).unwrap_or_default()
    }

    pub fn get_event_msgs(&self) -> Option<Vec<EventMsg>> {
        let items = self.rollout_items()?;
        Some(
            items
                .iter()
                .filter_map(|ri| match ri {
                    RolloutItem::EventMsg(ev) => Some(ev.clone()),
                    _ => None,
                })
                .collect(),
        )
    }

    pub fn get_base_instructions(&self) -> Option<BaseInstructions> {
        // TODO: SessionMeta should (in theory) always be first in the history, so we can probably only check the first item?
        self.find_session_meta()?.base_instructions.clone()
    }

    pub fn get_dynamic_tools(&self) -> Option<Vec<DynamicToolSpec>> {
        self.find_session_meta()?.dynamic_tools.clone()
    }

    /// The protected-context snapshot taken when the session was created.
    /// A resumed or forked session keeps this text even if the files it was
    /// read from have changed since.
    pub fn get_protected_context(&self) -> Option<ProtectedContext> {
        self.find_session_meta()?.protected_context.clone()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    Cli,
    #[default]
    VSCode,
    Exec,
    Mcp,
    Api,
    Acp,
    SubAgent(SubAgentSource),
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
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
            SessionSource::Api => f.write_str("api"),
            SessionSource::Acp => f.write_str("acp"),
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
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_persisted_dynamic_tools"
    )]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<String>,
    /// Text read once from `protected_context_files` when the session was
    /// created. Rendered into every initial context, including the one
    /// rebuilt after a compaction, so it is never summarised away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_context: Option<ProtectedContext>,
}

/// A snapshot of operator-chosen files that must survive compaction verbatim.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ProtectedContext {
    pub files: Vec<ProtectedContextFile>,
    /// True when the configured byte cap cut the snapshot short.
    pub truncated: bool,
}

/// One file's contents as read at session start.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ProtectedContextFile {
    /// The path as configured (relative paths are kept relative).
    pub path: String,
    pub text: String,
}

impl ProtectedContext {
    /// Whether the snapshot carries any text to render.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && !self.truncated
    }
}

/// Keep old tool visibility fields readable only at the journal boundary.
/// New registrations deserialize DynamicToolSpec directly and reject aliases.
fn deserialize_persisted_dynamic_tools<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<DynamicToolSpec>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct PersistedTool {
        name: String,
        description: String,
        input_schema: serde_json::Value,
        defer_loading: Option<bool>,
        expose_to_context: Option<bool>,
    }

    Ok(
        Option::<Vec<PersistedTool>>::deserialize(deserializer)?.map(|tools| {
            tools
                .into_iter()
                .map(|tool| DynamicToolSpec {
                    name: tool.name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                    defer_loading: tool.defer_loading.unwrap_or_else(|| {
                        tool.expose_to_context
                            .map(|visible| !visible)
                            .unwrap_or(false)
                    }),
                })
                .collect()
        }),
    )
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
            protected_context: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
pub struct SessionMetaLine {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

#[derive(Serialize, Debug, Clone, JsonSchema)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    ResponseItem(ResponseItem),
    Compacted(CompactedItem),
    CompactionControl(CompactionControlItem),
    TurnContext(TurnContextItem),
    EventMsg(EventMsg),
    BackgroundTask(crate::background_tasks::TaskJournalEvent),
    GoalCheckpoint(GoalCheckpointItem),
    /// A context window the transport reported for a model the catalog does
    /// not describe, recorded when it was received.
    ObservedModelContextWindow(ObservedModelContextWindowItem),
}

impl<'de> Deserialize<'de> for RolloutItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", content = "payload", rename_all = "snake_case")]
        enum RolloutItemWire {
            SessionMeta(SessionMetaLine),
            ResponseItem(ResponseItem),
            Compacted(CompactedItem),
            CompactionControl(CompactionControlItem),
            TurnContext(TurnContextItem),
            EventMsg(Value),
            BackgroundTask(crate::background_tasks::TaskJournalEvent),
            GoalCheckpoint(GoalCheckpointItem),
            ObservedModelContextWindow(ObservedModelContextWindowItem),
        }

        let item = RolloutItemWire::deserialize(deserializer)?;
        match item {
            RolloutItemWire::SessionMeta(item) => Ok(Self::SessionMeta(item)),
            RolloutItemWire::ResponseItem(item) => Ok(Self::ResponseItem(item)),
            RolloutItemWire::Compacted(item) => Ok(Self::Compacted(item)),
            RolloutItemWire::CompactionControl(item) => Ok(Self::CompactionControl(item)),
            RolloutItemWire::TurnContext(item) => Ok(Self::TurnContext(item)),
            RolloutItemWire::BackgroundTask(item) => Ok(Self::BackgroundTask(item)),
            RolloutItemWire::GoalCheckpoint(item) => Ok(Self::GoalCheckpoint(item)),
            RolloutItemWire::ObservedModelContextWindow(item) => {
                Ok(Self::ObservedModelContextWindow(item))
            }
            RolloutItemWire::EventMsg(value) => {
                let is_retired_undo_event = value
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "undo_started" | "undo_completed"));
                match serde_json::from_value(value) {
                    Ok(event) => Ok(Self::EventMsg(event)),
                    Err(_) if is_retired_undo_event => Ok(Self::ResponseItem(ResponseItem::Other)),
                    Err(error) => Err(<D::Error as serde::de::Error>::custom(error)),
                }
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct GoalCheckpointItem {
    pub conversation_id: ProcessId,
    pub server: String,
    pub endpoint: String,
    pub version: u32,
    pub snapshot: Value,
    pub verdict: Option<Value>,
}

/// A context window observed from the transport for `model` under the
/// `provider` binding (clamp slugs have no catalog entry). Persisted when the
/// observation arrives, so a fresh process resuming the session has it
/// before its first request.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ObservedModelContextWindowItem {
    pub provider: String,
    pub model: String,
    pub context_window: i64,
    pub scope: ModelContextWindowScope,
}

/// Endpoint routing identity for a model-window observation. The enclosing
/// record's provider id identifies the configured account.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash, JsonSchema)]
pub struct ModelContextWindowScope {
    pub wire_api: String,
    pub base_url: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompactionControlAction {
    CompactNow,
    DeferOnce,
}

/// Durable record of an accepted agent decision for one compaction-pressure
/// window. The monotonically increasing window number is the resume-stable
/// identity; the UUID is retained only for tracing and stale live-call checks.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CompactionControlItem {
    pub window_number: u64,
    pub window_id: String,
    pub action: CompactionControlAction,
    pub model: String,
    pub effective_context_window: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferral_ceiling: Option<i64>,
    pub active_tokens: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
pub struct CompactedItem {
    pub message: String,
    pub replacement_history: Vec<ResponseItem>,
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct TurnContextNetworkItem {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
}

/// Persist once per real user turn after computing that turn's model-visible
/// context updates, and again after mid-turn compaction when replacement
/// history re-establishes full context, so resume/fork replay can recover the
/// latest durable baseline.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
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
    /// Provider/account used for this turn.
    pub model_provider: String,
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
    pub developer_instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_output_json_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_policy: Option<TruncationPolicy>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
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

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
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

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
    pub network_proxy: Option<SessionNetworkProxyRuntime>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ProcessNameUpdatedEvent {
    #[serde(rename = "process_id")]
    pub process_id: ProcessId,
    #[serde(
        rename = "process_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub process_name: Option<String>,
}
