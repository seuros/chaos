//! Chaos Warden — tool orchestration and approval flow.
//!
//! The Warden manages tool runtimes (shell, apply_patch, MCP calls),
//! coordinates the approval pipeline (user prompts, watchdog auto-review,
//! cached approvals), and handles sandbox escalation/retry logic.
//!
//! Extracted from codex-core's tools/ module. The kernel dispatches tool
//! requests to the Warden; the Warden decides how to execute them.
