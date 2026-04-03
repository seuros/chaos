//! Chaos Minions — worker sub-agents spawned by the kernel.
//!
//! Minions handle delegated tasks: background research, parallel tool
//! execution, multi-file editing, and autonomous sub-sessions. Each minion
//! runs in its own sandboxed context with scoped permissions inherited from
//! the parent session.
//!
//! Replaces upstream's "agents" concept with explicit lifecycle management
//! and configurable autonomy levels.

const NAMES: &str = include_str!("names.txt");

/// Returns the canonical list of minion names.
pub fn default_names() -> Vec<&'static str> {
    NAMES
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

/// Resolves nickname candidates for a role. Uses `role_candidates` when
/// provided, otherwise falls back to the canonical minion name list.
pub fn nickname_candidates(role_candidates: Option<Vec<String>>) -> Vec<String> {
    if let Some(candidates) = role_candidates {
        return candidates;
    }
    default_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}
