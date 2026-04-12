use chaos_scm::GhostCommit;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use super::function_call::FunctionCallOutputPayload;
use super::shell::LocalShellAction;
use super::shell::LocalShellStatus;
use super::shell::ReasoningItemContent;
use super::shell::ReasoningItemReasoningSummary;
use super::shell::WebSearchAction;
use crate::mcp::CallToolResult;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ContentItem>,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tool_name: Option<String>,
    },
    McpToolCallOutput {
        call_id: String,
        output: CallToolResult,
    },
    CustomToolCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tool_name: Option<String>,
    },
    ToolSearchOutput {
        call_id: String,
        status: String,
        execution: String,
        #[ts(type = "unknown[]")]
        tools: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    OutputText { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
/// Classifies an assistant message as interim commentary or final answer text.
///
/// Providers do not emit this consistently, so callers must treat `None` as
/// "phase unknown" and keep compatibility behavior for legacy models.
pub enum MessagePhase {
    /// Mid-turn assistant text (for example preamble/progress narration).
    ///
    /// Additional tool calls or assistant output may follow before turn
    /// completion.
    Commentary,
    /// The assistant's terminal answer text for the current turn.
    FinalAnswer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
        // Do not use directly, no available consistently across all providers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        end_turn: Option<bool>,
        // Optional output-message phase (for example: "commentary", "final_answer").
        // Availability varies by provider/model, so downstream consumers must
        // preserve fallback behavior when this is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        phase: Option<MessagePhase>,
    },
    Reasoning {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: String,
        summary: Vec<ReasoningItemReasoningSummary>,
        #[serde(default, skip_serializing_if = "should_serialize_reasoning_content")]
        #[ts(optional)]
        content: Option<Vec<ReasoningItemContent>>,
        encrypted_content: Option<String>,
    },
    LocalShellCall {
        /// Legacy id field retained for compatibility with older payloads.
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        /// Set when using the Responses API.
        call_id: Option<String>,
        status: LocalShellStatus,
        action: LocalShellAction,
    },
    FunctionCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        namespace: Option<String>,
        // The Responses API returns the function call arguments as a *string* that contains
        // JSON, not as an already‑parsed object. We keep it as a raw string here and let
        // Session::handle_function_call parse it into a Value.
        arguments: String,
        call_id: String,
    },
    ToolSearchCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        execution: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
    },
    // NOTE: The `output` field for `function_call_output` uses a dedicated payload type with
    // custom serialization. On the wire it is either:
    //   - a plain string (`content`)
    //   - an array of structured content items (`content_items`)
    // We keep this behavior centralized in `FunctionCallOutputPayload`.
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tool_name: Option<String>,
    },
    CustomToolCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,

        call_id: String,
        name: String,
        input: String,
    },
    // `custom_tool_call_output.output` uses the same wire encoding as
    // `function_call_output.output` so freeform tools can return either plain
    // text or structured content items.
    CustomToolCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tool_name: Option<String>,
    },
    ToolSearchOutput {
        call_id: Option<String>,
        status: String,
        execution: String,
        #[ts(type = "unknown[]")]
        tools: Vec<serde_json::Value>,
    },
    // Emitted by the Responses API when the agent triggers a web search.
    // Example payload (from SSE `response.output_item.done`):
    // {
    //   "id":"ws_...",
    //   "type":"web_search_call",
    //   "status":"completed",
    //   "action": {"type":"search","query":"weather: San Francisco, CA"}
    // }
    WebSearchCall {
        #[serde(default, skip_serializing)]
        #[ts(skip)]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        action: Option<WebSearchAction>,
    },
    // Emitted by the Responses API when the agent triggers image generation.
    // Example payload:
    // {
    //   "id":"ig_123",
    //   "type":"image_generation_call",
    //   "status":"completed",
    //   "revised_prompt":"A gray tabby cat hugging an otter...",
    //   "result":"..."
    // }
    ImageGenerationCall {
        id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        revised_prompt: Option<String>,
        result: String,
    },
    // Generated by the harness but considered exactly as a model response.
    GhostSnapshot {
        ghost_commit: GhostCommit,
    },
    #[serde(alias = "compaction_summary")]
    Compaction {
        encrypted_content: String,
    },
    #[serde(other)]
    Other,
}

fn should_serialize_reasoning_content(content: &Option<Vec<ReasoningItemContent>>) -> bool {
    match content {
        Some(content) => !content
            .iter()
            .any(|c| matches!(c, ReasoningItemContent::ReasoningText { .. })),
        None => false,
    }
}

impl From<ResponseInputItem> for ResponseItem {
    fn from(item: ResponseInputItem) -> Self {
        match item {
            ResponseInputItem::Message { role, content } => Self::Message {
                role,
                content,
                id: None,
                end_turn: None,
                phase: None,
            },
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output,
                tool_name,
            } => Self::FunctionCallOutput {
                call_id,
                output,
                tool_name,
            },
            ResponseInputItem::McpToolCallOutput { call_id, output } => {
                let output = output.into_function_call_output_payload();
                Self::FunctionCallOutput {
                    call_id,
                    output,
                    tool_name: None,
                }
            }
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                output,
                tool_name,
            } => Self::CustomToolCallOutput {
                call_id,
                output,
                tool_name,
            },
            ResponseInputItem::ToolSearchOutput {
                call_id,
                status,
                execution,
                tools,
            } => Self::ToolSearchOutput {
                call_id: Some(call_id),
                status,
                execution,
                tools,
            },
        }
    }
}
