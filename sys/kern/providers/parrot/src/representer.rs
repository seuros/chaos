//! Wire-format representers — projection layer between Chaos-ABI and provider wire formats.
//!
//! Chaos-ABI uses `"system"` as the canonical instruction role.  Each provider
//! adapter owns a [`SessionRepresenter`] that knows how to project ABI items
//! into the wire format that provider expects, including any role remapping.
//!
//! ## Canonical role mapping
//!
//! | Wire role     | Chaos-ABI role | Provider              |
//! |---------------|----------------|-----------------------|
//! | `developer`   | `system`       | OpenAI Responses API  |
//! | `system`      | `system`       | xAI, compat clones    |
//!
//! OpenAI's Responses API introduced `developer` as its alias for system-level
//! instructions.  That is an OpenAI-specific wire detail — Chaos-ABI does not
//! expose it.  The [`ResponsesRepresenter`] remaps `system` → `developer` on
//! the way out.  The [`OpenwAInnabeRepresenter`] lets `system` pass through.

use std::fmt;
use std::sync::Arc;

use chaos_ipc::models::ResponseItem;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Projects a sequence of Chaos-ABI response items into provider-safe wire items.
pub trait Representer: Send + Sync {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem>;
}

// ---------------------------------------------------------------------------
// Shared base transforms (private)
// ---------------------------------------------------------------------------

/// Common projection applied by all Responses-API-compatible representers.
///
/// - `CustomToolCall` → `FunctionCall`
/// - `CustomToolCallOutput` → `FunctionCallOutput`
/// - `LocalShellCall` → `FunctionCall("shell_command")`
/// - Strips `tool_name` from output variants (ABI extension, not in OpenAI schema)
/// - Drops: `ToolSearchCall`, `ToolSearchOutput`, `GhostSnapshot`, `Compaction`, `Other`
fn base_represent(item: ResponseItem) -> Option<ResponseItem> {
    match item {
        // Standard output — strip the ABI-internal tool_name field.
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

        // LocalShellCall → FunctionCall so the matching FunctionCallOutput
        // is not orphaned when the representer runs.
        ResponseItem::LocalShellCall {
            id,
            call_id,
            action,
            status: _,
        } => {
            let call_id = call_id.unwrap_or_default();
            let arguments = serde_json::to_string(&action).unwrap_or_default();
            Some(ResponseItem::FunctionCall {
                id: id.or_else(|| Some(call_id.clone())),
                name: "shell_command".to_string(),
                namespace: None,
                arguments,
                call_id,
            })
        }

        // Chaos-only types with no OpenAI equivalent — drop them.
        ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => None,

        // Everything else passes through to the per-representer stage.
        other => Some(other),
    }
}

// ---------------------------------------------------------------------------
// ResponsesRepresenter — real OpenAI
// ---------------------------------------------------------------------------

/// Representer for the real OpenAI Responses API.
///
/// Applies [`base_represent`] then remaps the Chaos-ABI `"system"` role to
/// `"developer"`, which is the role OpenAI's Responses API requires for
/// system-level instructions.  `Reasoning` items and all other OpenAI
/// extensions pass through untouched.
pub struct ResponsesRepresenter;

impl Representer for ResponsesRepresenter {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        items
            .into_iter()
            .filter_map(base_represent)
            .map(remap_system_to_developer)
            .collect()
    }
}

fn remap_system_to_developer(item: ResponseItem) -> ResponseItem {
    match item {
        ResponseItem::Message {
            id,
            role,
            content,
            end_turn,
            phase,
        } if role == "system" => ResponseItem::Message {
            id,
            role: "developer".to_string(),
            content,
            end_turn,
            phase,
        },
        other => other,
    }
}

// ---------------------------------------------------------------------------
// OpenwAInnabeRepresenter — xAI and compat clones
// ---------------------------------------------------------------------------

/// Representer for providers that speak the OpenAI Responses API dialect but
/// diverge from it — xAI/Grok being the founding member.
///
/// Applies [`base_represent`] then drops `Reasoning` items, which rely on
/// OpenAI's `encrypted_content` mechanism for cross-turn context restoration
/// that wannabe providers do not implement.
///
/// The Chaos-ABI `"system"` role passes through unchanged — xAI accepts
/// `"system"` natively and does not use the `"developer"` alias.
pub struct OpenwAInnabeRepresenter;

impl Representer for OpenwAInnabeRepresenter {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        items
            .into_iter()
            .filter_map(base_represent)
            .filter(|item| !matches!(item, ResponseItem::Reasoning { .. }))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// SessionRepresenter — session-scoped wrapper
// ---------------------------------------------------------------------------

/// Session-scoped representer handle.
///
/// Wraps an `Arc<dyn Representer>` so it can be cheaply cloned across retry
/// loops without re-constructing the underlying representer.  Created once at
/// session initialisation based on the provider identity.
#[derive(Clone)]
pub struct SessionRepresenter(Arc<dyn Representer>);

impl SessionRepresenter {
    /// Representer for the real OpenAI Responses API.
    pub fn openai() -> Self {
        Self(Arc::new(ResponsesRepresenter))
    }

    /// Representer for OpenAI-compatible providers that diverge from the spec
    /// (xAI/Grok and future compat clones).
    pub fn wannabe() -> Self {
        Self(Arc::new(OpenwAInnabeRepresenter))
    }

    /// Project a batch of ABI items for wire serialization.
    pub fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        self.0.represent(items)
    }

    /// Access the inner representer for callers that need `&dyn Representer`.
    pub fn as_representer(&self) -> &dyn Representer {
        self.0.as_ref()
    }
}

impl fmt::Debug for SessionRepresenter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionRepresenter")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::models::FunctionCallOutputPayload;

    // --- ResponsesRepresenter (real OpenAI) ---------------------------------

    #[test]
    fn openai_representer_remaps_system_to_developer() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];
        let result = ResponsesRepresenter.represent(items);
        assert!(
            matches!(&result[0], ResponseItem::Message { role, .. } if role == "developer"),
            "real OpenAI must remap system → developer"
        );
    }

    #[test]
    fn openai_representer_passes_reasoning_items_through() {
        let items = vec![ResponseItem::Reasoning {
            id: "rs_1".into(),
            summary: vec![],
            content: None,
            encrypted_content: Some("enc".into()),
        }];
        let result = ResponsesRepresenter.represent(items);
        assert_eq!(result.len(), 1, "real OpenAI keeps Reasoning items");
    }

    #[test]
    fn openai_representer_passes_assistant_role_unchanged() {
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

    // --- OpenwAInnabeRepresenter (xAI and friends) --------------------------

    #[test]
    fn wannabe_passes_system_role_through() {
        let items = vec![ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];
        let result = OpenwAInnabeRepresenter.represent(items);
        assert!(
            matches!(&result[0], ResponseItem::Message { role, .. } if role == "system"),
            "wannabe must keep system role unchanged"
        );
    }

    #[test]
    fn wannabe_drops_reasoning_items() {
        let items = vec![ResponseItem::Reasoning {
            id: "rs_1".into(),
            summary: vec![],
            content: None,
            encrypted_content: Some("enc".into()),
        }];
        let result = OpenwAInnabeRepresenter.represent(items);
        assert!(result.is_empty(), "wannabe must drop Reasoning items");
    }

    #[test]
    fn wannabe_still_converts_custom_tool_call() {
        let items = vec![ResponseItem::CustomToolCall {
            id: None,
            status: None,
            call_id: "call_w".into(),
            name: "apply_patch".into(),
            input: r#"{"p":"x"}"#.into(),
        }];
        let result = OpenwAInnabeRepresenter.represent(items);
        assert!(
            matches!(&result[0], ResponseItem::FunctionCall { call_id, .. } if call_id == "call_w")
        );
    }

    // --- Shared base behaviour ----------------------------------------------

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
    fn local_shell_call_becomes_function_call() {
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
        match &result[0] {
            ResponseItem::FunctionCall { call_id, name, .. } => {
                assert_eq!(call_id, "sh_1");
                assert_eq!(name, "shell_command");
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }
    }

    // --- SessionRepresenter -------------------------------------------------

    #[test]
    fn session_representer_openai_remaps_system() {
        let sr = SessionRepresenter::openai();
        let items = vec![ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];
        let result = sr.represent(items);
        assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "developer"));
    }

    #[test]
    fn session_representer_wannabe_keeps_system() {
        let sr = SessionRepresenter::wannabe();
        let items = vec![ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }];
        let result = sr.represent(items);
        assert!(matches!(&result[0], ResponseItem::Message { role, .. } if role == "system"));
    }

    #[test]
    fn session_representer_is_clone() {
        let sr = SessionRepresenter::openai();
        let sr2 = sr.clone();
        // both should work identically
        let items = || {
            vec![ResponseItem::Message {
                id: None,
                role: "system".into(),
                content: vec![],
                end_turn: None,
                phase: None,
            }]
        };
        assert_eq!(sr.represent(items()).len(), sr2.represent(items()).len());
    }
}
