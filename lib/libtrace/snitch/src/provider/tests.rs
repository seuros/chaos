use super::*;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

#[test]
fn resource_attributes_include_host_name_when_present() {
    let attrs = resource_attributes(
        &test_otel_settings(),
        Some("opentelemetry-test"),
        ResourceKind::Logs,
    );

    let host_name = attrs
        .iter()
        .find(|kv| kv.key.as_str() == HOST_NAME_ATTRIBUTE)
        .map(|kv| kv.value.as_str().to_string());

    assert_eq!(host_name, Some("opentelemetry-test".to_string()));
}

#[test]
fn resource_attributes_omit_host_name_when_missing_or_empty() {
    let missing = resource_attributes(&test_otel_settings(), None, ResourceKind::Logs);
    let empty = resource_attributes(&test_otel_settings(), Some("   "), ResourceKind::Logs);
    let trace_attrs = resource_attributes(
        &test_otel_settings(),
        Some("opentelemetry-test"),
        ResourceKind::Traces,
    );

    assert!(
        !missing
            .iter()
            .any(|kv| kv.key.as_str() == HOST_NAME_ATTRIBUTE)
    );
    assert!(
        !empty
            .iter()
            .any(|kv| kv.key.as_str() == HOST_NAME_ATTRIBUTE)
    );
    assert!(
        !trace_attrs
            .iter()
            .any(|kv| kv.key.as_str() == HOST_NAME_ATTRIBUTE)
    );
}

#[test]
fn log_export_target_excludes_trace_safe_events() {
    assert!(is_log_export_target("chaos_snitch.log_only"));
    assert!(is_log_export_target("chaos_snitch.pf"));
    assert!(!is_log_export_target("chaos_snitch.trace_safe"));
    assert!(!is_log_export_target("chaos_snitch.trace_safe.debug"));
}

#[test]
fn trace_export_target_only_includes_trace_safe_prefix() {
    assert!(is_trace_safe_target("chaos_snitch.trace_safe"));
    assert!(is_trace_safe_target("chaos_snitch.trace_safe.summary"));
    assert!(!is_trace_safe_target("chaos_snitch.log_only"));
    assert!(!is_trace_safe_target("chaos_snitch.pf"));
}

fn test_otel_settings() -> OtelSettings {
    OtelSettings {
        environment: "test".to_string(),
        service_name: "chaos-test".to_string(),
        service_version: "0.0.0".to_string(),
        chaos_home: PathBuf::from("."),
        exporter: OtelExporter::None,
        trace_exporter: OtelExporter::None,
        metrics_exporter: OtelExporter::None,
        runtime_metrics: false,
    }
}
