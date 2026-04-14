//! Runtime database access trait.
//!
//! This stays backend-agnostic so consumers can ask whether runtime persistence
//! is available without depending on a concrete SQLite or Postgres handle type.

/// Reports whether runtime persistence is available for the current session.
pub trait RuntimeAccess: Send + Sync {
    fn has_runtime_db(&self) -> bool;
}
