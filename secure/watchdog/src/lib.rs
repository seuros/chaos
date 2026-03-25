//! Chaos Watchdog — risk scoring and automatic approval gate.
//!
//! The Watchdog evaluates risky operations (shell commands, patches, network
//! access, MCP tool calls) and auto-approves low-risk actions without user
//! intervention. High-risk actions are denied. Fail-closed by design.
//!
//! Replaces upstream's "guardian" concept with a cleaner separation: the
//! Watchdog only scores risk and makes yes/no decisions. It does not contain
//! agent logic or orchestration.

pub mod approval_request;
