//! OpenAI-specific Lark Schema Definitions.
//!
//! These extensions rely on `type: "custom"` tools in the OpenAI Responses
//! API with context-free grammar (CFG) constraints. They are inert on every
//! other provider.

pub mod apply_patch;
