//! Application tracing API, backed by Rama.
//!
//! `subscriber` names the subscriber crate, not the upstream
//! `tracing::subscriber` module. Use `dispatcher` or
//! `subscriber::util::SubscriberInitExt` to install scoped subscribers.

pub use rama::telemetry::tracing::*;

#[cfg(test)]
mod tests;
