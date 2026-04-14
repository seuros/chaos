//! MCP connection/session runtime.
//!
//! Owns [`McpConnectionManager`] and exposes kernel-facing DTOs so that
//! `chaos-kern` has zero direct dependency on `mcp-guest` protocol types.
//!
//! # Crate layout
//!
//! - [`types`]        — kernel-facing domain types (`ToolDescriptor`, `ToolCallResult`, …)
//! - [`manager`]      — `McpConnectionManager`
//! - [`catalog_conv`] — conversions from `mcp_guest` protocol objects to `chaos_traits` catalog types

pub mod catalog_conv;
pub mod manager;
pub mod types;

pub use manager::McpConnectionManager;
pub use manager::McpToolInfo;
pub use manager::SandboxState;
pub use manager::ToolFilter;
pub use manager::ToolInfo;

// Re-export mcp_guest protocol types used by chaos-kern so it does not
// need a direct mcp-guest dependency.
pub use mcp_guest::ListResourceTemplatesResult;
pub use mcp_guest::ListResourcesResult;
pub use mcp_guest::ListTasksResult;
pub use mcp_guest::PaginatedRequestParams;
pub use mcp_guest::ReadResourceRequestParams;
pub use mcp_guest::ReadResourceResult;
pub use mcp_guest::ResourceContents;
pub use mcp_guest::ResourceContentsText;
pub use mcp_guest::ResourceInfo;
pub use mcp_guest::ResourceTemplateInfo;
pub use mcp_guest::ToolAnnotations;
pub use mcp_guest::protocol::CallToolResult as McpToolCallResult;
pub use mcp_guest::protocol::ElicitationAction;
pub use mcp_guest::protocol::ElicitationResponse;
pub use mcp_guest::protocol::RequestId as McpRequestId;
pub use mcp_guest::protocol::Task as McpTask;
pub use mcp_guest::protocol::TaskSupport as McpTaskSupport;
pub use mcp_guest::protocol::ToolExecution as McpToolExecution;
