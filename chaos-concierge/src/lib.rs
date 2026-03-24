#![warn(rust_2024_compatibility, clippy::all)]

//! MCP connection concierge — server lifecycle, guest check-in/check-out, tool routing.
//!
//! The concierge manages all MCP server connections. It handles:
//!
//! - **Lifecycle** — spawn, initialize, health-check, and teardown MCP servers.
//! - **Check-in/Check-out** — register and deregister MCP guests dynamically.
//! - **Routing** — dispatch tool calls and resource requests to the correct server.

pub mod auth;
