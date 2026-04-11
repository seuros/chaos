//! OpenAI CFG-constrained apply_patch tool (Freeform variant).
//!
//! Uses `type: "custom"` with a Lark context-free grammar to constrain
//! model output directly on the OpenAI Responses API. Only works against
//! OpenAI — every other provider ignores or rejects `type: "custom"`.
//!
//! Reference: <https://platform.openai.com/docs/guides/function-calling#custom-tools>

/// The Lark grammar that constrains model output for freeform apply_patch.
/// Sent verbatim to OpenAI as the `format.definition` field.
pub const LARK_GRAMMAR: &str = include_str!("tool_apply_patch.lark");

/// Tool name used in the Responses API request.
pub const TOOL_NAME: &str = "apply_patch";

/// Tool description sent to the model when using the freeform variant.
pub const TOOL_DESCRIPTION: &str = "Use the `apply_patch` tool to edit files. \
    This is a FREEFORM tool, so do not wrap the patch in JSON.";

/// Format type sent in the `format` object.
pub const FORMAT_TYPE: &str = "grammar";

/// Grammar syntax identifier.
pub const FORMAT_SYNTAX: &str = "lark";
