use crate::harness::build_metrics_with_defaults;
use crate::harness::counter_attributes;
use crate::harness::latest_metrics;
use chaos_ipc::ProcessId;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::Result;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

// Ensures SessionTelemetry attaches metadata tags when forwarding metrics.
#[test]
fn manager_attaches_metadata_tags_to_metrics() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[("service", "chaos-cli")])?;
    let manager = SessionTelemetry::new(
        ProcessId::new(),
        "gpt-5.1",
        "gpt-5.1",
        Some(TelemetryAuthMode::ApiKey),
        "test_originator".to_string(),
        true,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics(metrics);

    manager.counter("chaos.session_started", 1, &[("source", "tui")]);
    manager.shutdown_metrics()?;

    let resource_metrics = latest_metrics(&exporter);
    let attrs = counter_attributes(&resource_metrics, "chaos.session_started");

    let expected = BTreeMap::from([
        ("app.version".to_string(), CHAOS_VERSION.to_string()),
        (
            "auth_mode".to_string(),
            TelemetryAuthMode::ApiKey.to_string(),
        ),
        ("model".to_string(), "gpt-5.1".to_string()),
        ("originator".to_string(), "test_originator".to_string()),
        ("service".to_string(), "chaos-cli".to_string()),
        ("session_source".to_string(), "cli".to_string()),
        ("source".to_string(), "tui".to_string()),
    ]);
    assert_eq!(attrs, expected);

    Ok(())
}

// Ensures metadata tagging can be disabled when recording via SessionTelemetry.
#[test]
fn manager_allows_disabling_metadata_tags() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;
    let manager = SessionTelemetry::new(
        ProcessId::new(),
        "gpt-4o",
        "gpt-4o",
        Some(TelemetryAuthMode::ApiKey),
        "test_originator".to_string(),
        true,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics_without_metadata_tags(metrics);

    manager.counter("chaos.session_started", 1, &[("source", "tui")]);
    manager.shutdown_metrics()?;

    let resource_metrics = latest_metrics(&exporter);
    let attrs = counter_attributes(&resource_metrics, "chaos.session_started");

    let expected = BTreeMap::from([("source".to_string(), "tui".to_string())]);
    assert_eq!(attrs, expected);

    Ok(())
}

#[test]
fn manager_attaches_optional_service_name_tag() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;
    let manager = SessionTelemetry::new(
        ProcessId::new(),
        "gpt-5.1",
        "gpt-5.1",
        None,
        "test_originator".to_string(),
        false,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics_service_name("my_app_server_client")
    .with_metrics(metrics);

    manager.counter("chaos.session_started", 1, &[]);
    manager.shutdown_metrics()?;

    let resource_metrics = latest_metrics(&exporter);
    let attrs = counter_attributes(&resource_metrics, "chaos.session_started");

    assert_eq!(
        attrs.get("service_name"),
        Some(&"my_app_server_client".to_string())
    );

    Ok(())
}
