//! Wire-format representers — Trailblazer-style projection layer.
//!
//! Each provider speaks a different dialect.  The representer's job is to
//! project Chaos-ABI types (`ResponseItem`) into the subset that a
//! particular wire format accepts, stripping internal extensions the
//! upstream API would reject.
//!
//! Anthropic and ChatCompletions adapters already perform full structural
//! conversion (ABI → `AnthropicMessage` / `ChatMessage`), so they don't
//! need a representer pass.  The OpenAI Responses API, however, serializes
//! `ResponseItem` directly via serde — meaning any ABI-internal field
//! that isn't `skip_serializing_if` guarded will leak to the wire.
//!
//! The [`Representer`] trait formalises this projection so every adapter
//! has a single, testable place where "what the wire sees" is decided.

use chaos_ipc::models::ResponseItem;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Projects a sequence of ABI response items into provider-safe wire items.
///
/// Implementors strip, rename, or reshape fields that are Chaos-internal
/// and would be rejected (or silently misinterpreted) by the upstream API.
pub trait Representer {
    /// Project a batch of items for serialization.
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem>;
}

// ---------------------------------------------------------------------------
// OpenAI Responses API representer
// ---------------------------------------------------------------------------

/// Representer for the OpenAI Responses API (and compatible providers).
///
/// Normalises Chaos-ABI items to the subset the Responses API accepts:
///
/// - `CustomToolCall` → `FunctionCall`: chaos uses a dedicated variant for
///   freeform/custom tools but OpenAI-compat providers only understand the
///   standard `function_call` wire type.
/// - `CustomToolCallOutput` → `FunctionCallOutput`: same reason; the
///   `custom_tool_call_output` type is Chaos-internal and causes 422s on
///   providers that validate against the OpenAI schema (e.g. xAI/Grok).
/// - `tool_name` stripped from all output variants — an ABI extension not
///   present in the OpenAI schema.
/// - Chaos-only items (`LocalShellCall`, `ToolSearchCall`, `ToolSearchOutput`,
///   `GhostSnapshot`, `Compaction`, `Other`) are dropped; they have no
///   OpenAI equivalent and would be rejected.
pub struct ResponsesRepresenter;

impl Representer for ResponsesRepresenter {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        items
            .into_iter()
            .filter_map(represent_for_responses)
            .collect()
    }
}

fn represent_for_responses(item: ResponseItem) -> Option<ResponseItem> {
    match item {
        // Standard output — only strip the ABI-internal tool_name field.
        ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name: _,
        } => Some(ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name: None,
        }),

        // Freeform-tool output → standard function_call_output.
        ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            tool_name: _,
        } => Some(ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name: None,
        }),

        // Freeform-tool call → standard function_call.
        ResponseItem::CustomToolCall {
            id,
            call_id,
            name,
            input,
            status: _,
        } => Some(ResponseItem::FunctionCall {
            id,
            name,
            namespace: None,
            arguments: input,
            call_id,
        }),

        // Chaos-only types with no OpenAI equivalent — drop them.
        ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => None,

        // Everything else (Message, Reasoning, FunctionCall, WebSearchCall,
        // ImageGenerationCall) passes through unchanged.
        other => Some(other),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::models::FunctionCallOutputPayload;

    #[test]
    fn strips_tool_name_from_function_call_output() {
        let items = vec![ResponseItem::FunctionCallOutput {
            call_id: "call_1".into(),
            output: FunctionCallOutputPayload::from_text("ok".into()),
            tool_name: Some("read_file".into()),
        }];

        let result = ResponsesRepresenter.represent(items);

        match &result[0] {
            ResponseItem::FunctionCallOutput { tool_name, .. } => {
                assert!(tool_name.is_none());
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn custom_tool_call_output_becomes_function_call_output() {
        let items = vec![ResponseItem::CustomToolCallOutput {
            call_id: "call_2".into(),
            output: FunctionCallOutputPayload::from_text("patched".into()),
            tool_name: Some("apply_patch".into()),
        }];

        let result = ResponsesRepresenter.represent(items);

        match &result[0] {
            ResponseItem::FunctionCallOutput {
                call_id, tool_name, ..
            } => {
                assert_eq!(call_id, "call_2");
                assert!(tool_name.is_none());
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn custom_tool_call_becomes_function_call() {
        let items = vec![ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: "call_3".into(),
            name: "apply_patch".into(),
            input: r#"{"patch":"..."}"#.into(),
        }];

        let result = ResponsesRepresenter.represent(items);

        match &result[0] {
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call_3");
                assert_eq!(name, "apply_patch");
                assert_eq!(arguments, r#"{"patch":"..."}"#);
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    #[test]
    fn local_shell_call_is_dropped() {
        use chaos_ipc::models::{LocalShellAction, LocalShellExecAction, LocalShellStatus};
        let items = vec![ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("sh_1".into()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["ls".into()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
        }];

        let result = ResponsesRepresenter.represent(items);
        assert!(result.is_empty(), "LocalShellCall should be filtered out");
    }

    #[test]
    fn passes_through_message() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];

        let result = ResponsesRepresenter.represent(items);
        assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "assistant"));
    }
}
