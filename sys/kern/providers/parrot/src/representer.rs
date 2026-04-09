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

/// Representer for the OpenAI Responses API.
///
/// Strips Chaos-internal fields that OpenAI does not accept:
///
/// - `tool_name` on `FunctionCallOutput` and `CustomToolCallOutput` — an
///   ABI extension used by the kernel to correlate outputs back to tool
///   definitions.  OpenAI's schema has no such field and returns 400 if
///   it appears in the request.
pub struct ResponsesRepresenter;

impl Representer for ResponsesRepresenter {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        items.into_iter().map(represent_for_responses).collect()
    }
}

fn represent_for_responses(item: ResponseItem) -> ResponseItem {
    match item {
        ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name: _,
        } => ResponseItem::FunctionCallOutput {
            call_id,
            output,
            tool_name: None,
        },
        ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            tool_name: _,
        } => ResponseItem::CustomToolCallOutput {
            call_id,
            output,
            tool_name: None,
        },
        other => other,
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
                assert!(tool_name.is_none(), "tool_name should be stripped");
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn strips_tool_name_from_custom_tool_call_output() {
        let items = vec![ResponseItem::CustomToolCallOutput {
            call_id: "call_2".into(),
            output: FunctionCallOutputPayload::from_text("patched".into()),
            tool_name: Some("apply_patch".into()),
        }];

        let result = ResponsesRepresenter.represent(items);

        match &result[0] {
            ResponseItem::CustomToolCallOutput { tool_name, .. } => {
                assert!(tool_name.is_none(), "tool_name should be stripped");
            }
            other => panic!("expected CustomToolCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn passes_through_unaffected_variants() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];

        let result = ResponsesRepresenter.represent(items);

        assert!(
            matches!(&result[0], ResponseItem::Message { role, .. } if role == "assistant"),
            "Message items should pass through unchanged"
        );
    }
}
