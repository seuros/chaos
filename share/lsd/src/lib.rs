//! chaos-lsd — Lark Schema Definitions
//!
//! Vendor-specific tool extensions and schema definitions.
//! Each vendor module exposes provider-only behaviours (CFG-constrained
//! tools, custom wire formats, grammar definitions) that the portable kern
//! core deliberately knows nothing about.
//!
//! Current vendor modules:
//! - [`openai`] — OpenAI Responses API extensions (CFG/Lark grammar tools)

pub mod openai;
