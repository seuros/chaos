use crate::harness::build_metrics_with_defaults;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_syslog::metrics::MetricsConfig;
use chaos_syslog::metrics::MetricsError;
use chaos_syslog::metrics::Result;
use rama::telemetry::opentelemetry::sdk::metrics::InMemoryMetricExporter;

// Ensures invalid tag components are rejected during config build.
#[test]
fn invalid_tag_component_is_rejected() -> Result<()> {
    let err = MetricsConfig::in_memory(
        "test",
        "chaos-cli",
        CHAOS_VERSION,
        InMemoryMetricExporter::default(),
    )
    .with_tag("bad key", "value")
    .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag key" && value == "bad key"
    ));
    Ok(())
}

// Ensures per-metric tag keys are validated.
#[test]
fn counter_rejects_invalid_tag_key() -> Result<()> {
    let (metrics, _exporter) = build_metrics_with_defaults(&[])?;
    let err = metrics
        .counter("chaos.turns", 1, &[("bad key", "value")])
        .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag key" && value == "bad key"
    ));
    metrics.shutdown()?;
    Ok(())
}

// Ensures per-metric tag values are validated.
#[test]
fn histogram_rejects_invalid_tag_value() -> Result<()> {
    let (metrics, _exporter) = build_metrics_with_defaults(&[])?;
    let err = metrics
        .histogram("chaos.request_latency", 3, &[("route", "bad value")])
        .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag value" && value == "bad value"
    ));
    metrics.shutdown()?;
    Ok(())
}

// Ensures invalid metric names are rejected.
#[test]
fn counter_rejects_invalid_metric_name() -> Result<()> {
    let (metrics, _exporter) = build_metrics_with_defaults(&[])?;
    let err = metrics.counter("bad name", 1, &[]).unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidMetricName { name } if name == "bad name"
    ));
    metrics.shutdown()?;
    Ok(())
}

#[test]
fn counter_rejects_negative_increment() -> Result<()> {
    let (metrics, _exporter) = build_metrics_with_defaults(&[])?;
    let err = metrics.counter("chaos.turns", -1, &[]).unwrap_err();
    assert!(matches!(
        err,
        MetricsError::NegativeCounterIncrement { name, inc } if name == "chaos.turns" && inc == -1
    ));
    metrics.shutdown()?;
    Ok(())
}
