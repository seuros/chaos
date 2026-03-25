//! State database access trait — decouples consumers from the concrete SQLite handle.

use std::sync::Arc;

use codex_state::StateRuntime;

/// Provides access to the optional SQLite state database.
///
/// Returns `None` when the session is ephemeral or state persistence is disabled.
pub trait StateAccess: Send + Sync {
    fn state_db(&self) -> Option<Arc<StateRuntime>>;
}
