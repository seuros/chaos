pub(crate) const OTEL_TARGET_PREFIX: &str = "chaos_syslog";
pub(crate) const OTEL_LOG_ONLY_TARGET: &str = "chaos_syslog.log_only";
pub(crate) const OTEL_TRACE_SAFE_TARGET: &str = "chaos_syslog.trace_safe";

pub(crate) fn is_log_export_target(target: &str) -> bool {
    target.starts_with(OTEL_TARGET_PREFIX) && !is_trace_safe_target(target)
}

pub(crate) fn is_trace_safe_target(target: &str) -> bool {
    target.starts_with(OTEL_TRACE_SAFE_TARGET)
}
