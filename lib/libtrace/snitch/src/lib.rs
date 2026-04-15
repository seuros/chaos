//! Chaos Snitch — telemetry, observability, and audit trail.
//!
//! Structured events, local-first metrics, token usage tracking, and
//! session diagnostics. Replaces the generic chaos-otel with a
//! purpose-built observability layer. Opt-in remote reporting only.

#![warn(clippy::all)]

mod file_logging;

pub use file_logging::BoxedLogLayer;
pub use file_logging::open_debug_log_file_layer;
pub use file_logging::open_log_file_layer;
