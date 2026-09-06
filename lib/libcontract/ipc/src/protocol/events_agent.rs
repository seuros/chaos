use crate::ProcessId;
use crate::config_types::ModeKind;
use crate::items::TurnItem;
use crate::models::MessagePhase;
use crate::models::ResponseItem;
use crate::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelRerouteReason {
    /// Upstream provider silently substituted a different model than the
    /// one requested. Vendor-agnostic: covers OpenAI abuse heuristics,
    /// TensorZero routing rules, Anthropic fallbacks, etc.
    VendorDeclinedSelection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct ModelRerouteEvent {
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct ParentEffortChangedEvent {
    pub effort: ReasoningEffort,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct SessionModeChangedEvent {
    pub session_id: ProcessId,
    pub mode_id: String,
    pub mode_title: String,
    pub mode_kind: ModeKind,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContextCompactedEvent;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TurnCompleteEvent {
    pub turn_id: String,
    pub last_agent_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TurnStartedEvent {
    pub turn_id: String,
    // TODO(aibrahim): make this not optional
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub collaboration_mode_kind: ModeKind,
}

/// Approximate live progress for an in-flight model turn.
///
/// These counts are deliberately heuristic and must not be used for billing,
/// context accounting, or rate-limit decisions. They exist only to make long
/// running turns visibly alive in clients.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema)]
pub struct TurnProgressEvent {
    pub turn_id: String,
    pub approx_reasoning_tokens: i64,
    pub approx_output_tokens: i64,
    pub approx_total_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentMessageEvent {
    pub message: String,
    #[serde(default)]
    pub phase: Option<MessagePhase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UserMessageEvent {
    pub message: String,
    /// Image URLs sourced from `UserInput::Image`. These are safe
    /// to replay in legacy UI history events and correspond to images sent to
    /// the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// Local file paths sourced from `UserInput::LocalImage`. These are kept so
    /// the UI can reattach images when editing history, and should not be sent
    /// to the model or treated as API-ready URLs.
    #[serde(default)]
    pub local_images: Vec<std::path::PathBuf>,
    /// UI-defined spans within `message` used to render or persist special elements.
    #[serde(default)]
    pub text_elements: Vec<crate::user_input::TextElement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentReasoningSectionBreakEvent {
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RawResponseItemEvent {
    pub item: ResponseItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ItemStartedEvent {
    pub process_id: ProcessId,
    pub turn_id: String,
    pub item: TurnItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ItemCompletedEvent {
    pub process_id: ProcessId,
    pub turn_id: String,
    pub item: TurnItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct AgentMessageContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PlanDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReasoningContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReasoningRawContentDeltaEvent {
    pub process_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    // load with default value so it's backward compatible with the old format.
    #[serde(default)]
    pub content_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ProcessRolledBackEvent {
    /// Number of user turns that were removed from context.
    pub num_turns: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct BackgroundEventEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DeprecationNoticeEvent {
    /// Concise summary of what is deprecated.
    pub summary: String,
    /// Optional extra guidance, such as migration steps or rationale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StreamInfoEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TurnAbortedEvent {
    pub turn_id: Option<String>,
    pub reason: TurnAbortReason,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
    ReviewEnded,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
    pub action: crate::models::WebSearchAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ImageGenerationBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ImageGenerationEndEvent {
    pub call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revised_prompt: Option<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
}
