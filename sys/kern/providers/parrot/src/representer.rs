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

use chaos_abi::ReasoningEffort;
use chaos_abi::ResponseItem;

mod moonshootai;
pub(crate) use moonshootai::is_kimi_endpoint;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Projects a sequence of Chaos-ABI response items into provider-safe wire items.
pub trait Representer: Send + Sync {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem>;

    /// Project provider-neutral reasoning effort onto this provider's wire
    /// vocabulary.
    fn represent_reasoning_effort(&self, effort: ReasoningEffort) -> ReasoningEffort {
        effort
    }

    /// Apply endpoint-specific controls after the common Responses projection.
    fn prepare_request(&self, _request: &mut crate::common::ResponsesApiRequest) {}
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
/// - Drops: `ToolSearchCall`, `ToolSearchOutput`, `Other`
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
            provider_metadata: None,
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
                provider_metadata: None,
            })
        }

        // Chaos-only types with no OpenAI equivalent — drop them.
        ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
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

    fn represent_reasoning_effort(&self, effort: ReasoningEffort) -> ReasoningEffort {
        crate::common::effort_for_openai_wire(effort)
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
            .filter(|item| {
                !matches!(
                    item,
                    ResponseItem::Reasoning { .. }
                        | ResponseItem::Compaction { .. }
                        | ResponseItem::CompactionTrigger {}
                )
            })
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

    /// Select a compatible endpoint's dialect without treating it as OpenAI.
    pub fn for_compatible_endpoint(base_url: &str) -> Self {
        if moonshootai::is_kimi_endpoint(base_url) {
            Self(Arc::new(moonshootai::KimiRepresenter))
        } else {
            Self::wannabe()
        }
    }

    /// Project a batch of ABI items for wire serialization.
    pub fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        self.0.represent(items)
    }

    /// Project provider-neutral reasoning effort onto this session's provider
    /// wire vocabulary.
    pub fn represent_reasoning_effort(&self, effort: ReasoningEffort) -> ReasoningEffort {
        self.0.represent_reasoning_effort(effort)
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
mod tests;
