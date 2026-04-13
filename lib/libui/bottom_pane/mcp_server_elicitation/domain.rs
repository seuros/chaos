use std::path::PathBuf;

use chaos_ipc::ProcessId;
use chaos_ipc::approvals::ElicitationRequest;
use chaos_ipc::approvals::ElicitationRequestEvent;
use chaos_ipc::mcp::RequestId as McpRequestId;
use chaos_ipc::user_input::TextElement;
use serde_json::Value;

use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::scroll_state::ScrollState;

use super::parsing::{
    parse_fields_from_schema, parse_tool_approval_display_params, parse_tool_suggestion_request,
    tool_approval_supports_persist_mode,
};

pub(super) const ANSWER_PLACEHOLDER: &str = "Type your answer";
pub(super) const OPTIONAL_ANSWER_PLACEHOLDER: &str = "Type your answer (optional)";
pub(super) const FOOTER_SEPARATOR: &str = " | ";
pub(super) const MIN_COMPOSER_HEIGHT: u16 = 3;
pub(super) const MIN_OVERLAY_HEIGHT: u16 = 8;
pub(super) const APPROVAL_FIELD_ID: &str = "__approval";
pub(super) const APPROVAL_ACCEPT_ONCE_VALUE: &str = "accept";
pub(super) const APPROVAL_ACCEPT_SESSION_VALUE: &str = "accept_session";
pub(super) const APPROVAL_ACCEPT_ALWAYS_VALUE: &str = "accept_always";
pub(super) const APPROVAL_DECLINE_VALUE: &str = "decline";
pub(super) const APPROVAL_CANCEL_VALUE: &str = "cancel";
pub(super) const APPROVAL_META_KIND_KEY: &str = "codex_approval_kind";
pub(super) const APPROVAL_META_KIND_MCP_TOOL_CALL: &str = "mcp_tool_call";
pub(super) const APPROVAL_META_KIND_TOOL_SUGGESTION: &str = "tool_suggestion";
pub(super) const APPROVAL_PERSIST_KEY: &str = "persist";
pub(super) const APPROVAL_PERSIST_SESSION_VALUE: &str = "session";
pub(super) const APPROVAL_PERSIST_ALWAYS_VALUE: &str = "always";
pub(super) const APPROVAL_TOOL_PARAMS_KEY: &str = "tool_params";
pub(super) const APPROVAL_TOOL_PARAMS_DISPLAY_KEY: &str = "tool_params_display";
pub(super) const APPROVAL_TOOL_PARAM_DISPLAY_LIMIT: usize = 3;
pub(super) const APPROVAL_TOOL_PARAM_VALUE_TRUNCATE_GRAPHEMES: usize = 60;
pub(super) const TOOL_TYPE_KEY: &str = "tool_type";
pub(super) const TOOL_ID_KEY: &str = "tool_id";
pub(super) const TOOL_NAME_KEY: &str = "tool_name";
pub(super) const TOOL_SUGGEST_SUGGEST_TYPE_KEY: &str = "suggest_type";
pub(super) const TOOL_SUGGEST_REASON_KEY: &str = "suggest_reason";
pub(super) const TOOL_SUGGEST_INSTALL_URL_KEY: &str = "install_url";

#[derive(Clone, PartialEq, Default)]
pub(super) struct ComposerDraft {
    pub(super) text: String,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) local_image_paths: Vec<PathBuf>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl ComposerDraft {
    pub(super) fn text_with_pending(&self) -> String {
        if self.pending_pastes.is_empty() {
            return self.text.clone();
        }
        debug_assert!(
            !self.text_elements.is_empty(),
            "pending pastes should always have matching text elements"
        );
        let (expanded, _) = ChatComposer::expand_pending_pastes(
            &self.text,
            self.text_elements.clone(),
            &self.pending_pastes,
        );
        expanded
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct McpServerElicitationOption {
    pub(super) label: String,
    pub(super) description: Option<String>,
    pub(super) value: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum McpServerElicitationFieldInput {
    Select {
        options: Vec<McpServerElicitationOption>,
        default_idx: Option<usize>,
    },
    Text {
        secret: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct McpServerElicitationField {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) prompt: String,
    pub(super) required: bool,
    pub(super) input: McpServerElicitationFieldInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum McpServerElicitationResponseMode {
    FormContent,
    ApprovalAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSuggestionToolType {
    Connector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSuggestionType {
    Install,
    Enable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSuggestionRequest {
    pub tool_type: ToolSuggestionToolType,
    pub suggest_type: ToolSuggestionType,
    pub suggest_reason: String,
    pub tool_id: String,
    pub tool_name: String,
    pub install_url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct McpToolApprovalDisplayParam {
    pub(super) name: String,
    pub(super) value: Value,
    pub(super) display_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpServerElicitationFormRequest {
    pub(super) process_id: ProcessId,
    pub(super) server_name: String,
    pub(super) request_id: McpRequestId,
    pub(super) message: String,
    pub(super) approval_display_params: Vec<McpToolApprovalDisplayParam>,
    pub(super) response_mode: McpServerElicitationResponseMode,
    pub(super) fields: Vec<McpServerElicitationField>,
    pub(super) tool_suggestion: Option<ToolSuggestionRequest>,
}

#[derive(Default)]
pub(super) struct McpServerElicitationAnswerState {
    pub(super) selection: ScrollState,
    pub(super) draft: ComposerDraft,
    pub(super) answer_committed: bool,
}

impl McpServerElicitationFormRequest {
    pub fn from_event(process_id: ProcessId, request: ElicitationRequestEvent) -> Option<Self> {
        let ElicitationRequest::Form {
            meta,
            message,
            requested_schema,
        } = request.request
        else {
            return None;
        };

        let tool_suggestion = parse_tool_suggestion_request(meta.as_ref());
        let is_tool_approval = meta
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|meta| meta.get(APPROVAL_META_KIND_KEY))
            .and_then(Value::as_str)
            == Some(APPROVAL_META_KIND_MCP_TOOL_CALL);
        let is_empty_object_schema = requested_schema.as_object().is_some_and(|schema| {
            schema.get("type").and_then(Value::as_str) == Some("object")
                && schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(serde_json::Map::is_empty)
        });
        let is_tool_approval_action =
            is_tool_approval && (requested_schema.is_null() || is_empty_object_schema);
        let approval_display_params = if is_tool_approval_action {
            parse_tool_approval_display_params(meta.as_ref())
        } else {
            Vec::new()
        };

        let (response_mode, fields) = if tool_suggestion.is_some()
            && (requested_schema.is_null() || is_empty_object_schema)
        {
            (McpServerElicitationResponseMode::FormContent, Vec::new())
        } else if requested_schema.is_null() || (is_tool_approval && is_empty_object_schema) {
            let mut options = vec![McpServerElicitationOption {
                label: "Allow".to_string(),
                description: Some("Run the tool and continue.".to_string()),
                value: Value::String(APPROVAL_ACCEPT_ONCE_VALUE.to_string()),
            }];
            if is_tool_approval_action
                && tool_approval_supports_persist_mode(
                    meta.as_ref(),
                    APPROVAL_PERSIST_SESSION_VALUE,
                )
            {
                options.push(McpServerElicitationOption {
                    label: "Allow for this session".to_string(),
                    description: Some(
                        "Run the tool and remember this choice for this session.".to_string(),
                    ),
                    value: Value::String(APPROVAL_ACCEPT_SESSION_VALUE.to_string()),
                });
            }
            if is_tool_approval_action
                && tool_approval_supports_persist_mode(meta.as_ref(), APPROVAL_PERSIST_ALWAYS_VALUE)
            {
                options.push(McpServerElicitationOption {
                    label: "Always allow".to_string(),
                    description: Some(
                        "Run the tool and remember this choice for future tool calls.".to_string(),
                    ),
                    value: Value::String(APPROVAL_ACCEPT_ALWAYS_VALUE.to_string()),
                });
            }
            if is_tool_approval_action {
                options.push(McpServerElicitationOption {
                    label: "Cancel".to_string(),
                    description: Some("Cancel this tool call".to_string()),
                    value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                });
            } else {
                options.extend([
                    McpServerElicitationOption {
                        label: "Deny".to_string(),
                        description: Some("Decline this tool call and continue.".to_string()),
                        value: Value::String(APPROVAL_DECLINE_VALUE.to_string()),
                    },
                    McpServerElicitationOption {
                        label: "Cancel".to_string(),
                        description: Some("Cancel this tool call".to_string()),
                        value: Value::String(APPROVAL_CANCEL_VALUE.to_string()),
                    },
                ]);
            }
            (
                McpServerElicitationResponseMode::ApprovalAction,
                vec![McpServerElicitationField {
                    id: APPROVAL_FIELD_ID.to_string(),
                    label: String::new(),
                    prompt: String::new(),
                    required: true,
                    input: McpServerElicitationFieldInput::Select {
                        options,
                        default_idx: Some(0),
                    },
                }],
            )
        } else {
            (
                McpServerElicitationResponseMode::FormContent,
                parse_fields_from_schema(&requested_schema)?,
            )
        };

        Some(Self {
            process_id,
            server_name: request.server_name,
            request_id: request.id,
            message,
            approval_display_params,
            response_mode,
            fields,
            tool_suggestion,
        })
    }

    pub fn tool_suggestion(&self) -> Option<&ToolSuggestionRequest> {
        self.tool_suggestion.as_ref()
    }

    pub fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub fn server_name(&self) -> &str {
        self.server_name.as_str()
    }

    pub fn request_id(&self) -> &McpRequestId {
        &self.request_id
    }
}
