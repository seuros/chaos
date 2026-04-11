use crate::harness::build_runtime_metrics_with_defaults;
use crate::harness::counter_attributes;
use chaos_ipc::ProcessId;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::Result;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

#[test]
fn snapshot_collects_metrics_without_shutdown() -> Result<()> {
    let (metrics, exporter) = build_runtime_metrics_with_defaults(&[("service", "chaos-cli")])?;

    metrics.counter(
        "chaos.tool.call",
        1,
        &[("tool", "shell"), ("success", "true")],
    )?;

    let snapshot = metrics.snapshot()?;

    let attrs = counter_attributes(&snapshot, "chaos.tool.call");

    let expected = BTreeMap::from([
        ("service".to_string(), "chaos-cli".to_string()),
        ("success".to_string(), "true".to_string()),
        ("tool".to_string(), "shell".to_string()),
    ]);
    assert_eq!(attrs, expected);

    let finished = exporter
        .get_finished_metrics()
        .expect("finished metrics should be readable");
    assert!(finished.is_empty(), "expected no periodic exports yet");

    Ok(())
}

#[test]
fn manager_snapshot_metrics_collects_without_shutdown() -> Result<()> {
    let (metrics, _exporter) = build_runtime_metrics_with_defaults(&[("service", "chaos-cli")])?;
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

    manager.counter(
        "chaos.tool.call",
        1,
        &[("tool", "shell"), ("success", "true")],
    );

    let snapshot = manager.snapshot_metrics()?;
    let attrs = counter_attributes(&snapshot, "chaos.tool.call");

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
        ("success".to_string(), "true".to_string()),
        ("tool".to_string(), "shell".to_string()),
    ]);
    assert_eq!(attrs, expected);

    Ok(())
}
