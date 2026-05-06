//! Chaos Snitch — telemetry, observability, and audit trail.
//!
//! Structured events, local-first metrics, token usage tracking, and
//! session diagnostics. Opt-in remote OTLP reporting.

#![warn(clippy::all)]

mod file_logging;
pub mod runtime_db;

pub mod config;
mod events;
pub mod metrics;
pub mod provider;
pub mod trace_context;

mod otlp;
mod rama_otel_client;
mod targets;

use crate::metrics::MetricsError;
use crate::metrics::Result as MetricsResult;
use serde::Serialize;
use strum_macros::Display;

pub use file_logging::BoxedLogLayer;
pub use file_logging::open_debug_log_file_layer;
pub use file_logging::open_log_file_layer;
pub use runtime_db::LogDbLayer;
pub use runtime_db::start_runtime_db_layer;

pub use crate::events::session_telemetry::SessionTelemetry;
pub use crate::events::session_telemetry::SessionTelemetryMetadata;
pub use crate::metrics::runtime_metrics::RuntimeMetricTotals;
pub use crate::metrics::runtime_metrics::RuntimeMetricsSummary;
pub use crate::metrics::timer::Timer;
pub use crate::provider::OtelProvider;
pub use crate::trace_context::context_from_w3c_trace_context;
pub use crate::trace_context::current_span_trace_id;
pub use crate::trace_context::current_span_w3c_trace_context;
pub use crate::trace_context::set_parent_from_context;
pub use crate::trace_context::set_parent_from_w3c_trace_context;
pub use crate::trace_context::span_w3c_trace_context;
pub use crate::trace_context::traceparent_context_from_env;
pub use chaos_wchar::sanitize_metric_tag_value;

#[derive(Debug, Clone, Serialize, Display)]
#[serde(rename_all = "snake_case")]
pub enum ToolDecisionSource {
    Config,
    User,
}

/// Maps to core AuthMode to avoid a circular dependency on chaos-kern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
pub enum TelemetryAuthMode {
    ApiKey,
    Chatgpt,
}

/// Start a metrics timer using the globally installed metrics client.
pub fn start_global_timer(name: &str, tags: &[(&str, &str)]) -> MetricsResult<Timer> {
    let Some(metrics) = crate::metrics::global() else {
        return Err(MetricsError::ExporterDisabled);
    };
    metrics.start_timer(name, tags)
}
