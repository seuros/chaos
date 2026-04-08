//! Runtime database access trait — decouples consumers from the concrete SQLite handle.

use std::sync::Arc;

use chaos_proc::StateRuntime;

/// Provides access to the optional SQLite runtime database.
///
/// Returns `None` when the session is ephemeral or runtime persistence is disabled.
pub trait RuntimeAccess: Send + Sync {
    fn runtime_db(&self) -> Option<Arc<StateRuntime>>;
}
