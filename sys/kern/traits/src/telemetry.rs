//! Telemetry access trait — exposes session-scoped metrics and tracing.

pub use chaos_syslog::SessionTelemetry;

/// Provides access to the session-scoped telemetry handle.
pub trait TelemetrySource: Send + Sync {
    fn session_telemetry(&self) -> &SessionTelemetry;
}
