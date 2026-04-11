//! Anthropic-owned endpoints.
//!
//! Same rule as `openai`: these are strings in someone else's
//! infrastructure. Do not rewrite them during internal refactors.

/// Base URL for the Anthropic Messages API. Everything hangs off
/// `{API_BASE}/messages`.
pub const API_BASE: &str = "https://api.anthropic.com/v1";

/// Anthropic Messages API endpoint.
pub const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
