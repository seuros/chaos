//! Chaos Minions — worker sub-agents spawned by the kernel.
//!
//! Minions handle delegated tasks: background research, parallel tool
//! execution, multi-file editing, and autonomous sub-sessions. Each minion
//! runs in its own sandboxed context with scoped permissions inherited from
//! the parent session.
//!
//! Replaces upstream's "agents" concept with explicit lifecycle management
//! and configurable autonomy levels.
