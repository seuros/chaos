//! MCP connection/session runtime.
//!
//! Owns [`McpConnectionManager`] and exposes kernel-facing DTOs so that
//! `chaos-kern` has zero direct dependency on `mcp-guest` protocol types.
//!
//! # Crate layout
//!
//! - [`types`]   — kernel-facing domain types (`ToolDescriptor`, `ToolCallResult`, …)
//! - [`manager`] — `McpConnectionManager` (to be migrated from `chaos-kern`)

pub mod types;
