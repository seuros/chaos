use crate::harness::build_metrics_with_defaults;
use crate::harness::counter_attributes;
use crate::harness::counter_value;
use crate::harness::histogram_attributes;
use crate::harness::histogram_data;
use crate::harness::latest_metrics;
use chaos_syslog::metrics::Result;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

// Ensures counters/histograms render with default + per-call tags.
#[test]
fn send_builds_payload_with_tags_and_histograms() -> Result<()> {
    let (metrics, exporter) =
        build_metrics_with_defaults(&[("service", "chaos-cli"), ("env", "prod")])?;

    metrics.counter("chaos.turns", 1, &[("model", "gpt-5.1"), ("env", "dev")])?;
    metrics.histogram("chaos.tool_latency", 25, &[("tool", "shell")])?;
    metrics.shutdown()?;

    let resource_metrics = latest_metrics(&exporter);

    assert_eq!(counter_value(&resource_metrics, "chaos.turns"), 1);
    let counter_attributes = counter_attributes(&resource_metrics, "chaos.turns");

    let expected_counter_attributes = BTreeMap::from([
        ("service".to_string(), "chaos-cli".to_string()),
        ("env".to_string(), "dev".to_string()),
        ("model".to_string(), "gpt-5.1".to_string()),
    ]);
    assert_eq!(counter_attributes, expected_counter_attributes);

    let (bounds, bucket_counts, sum, count) =
        histogram_data(&resource_metrics, "chaos.tool_latency");
    assert!(!bounds.is_empty());
    assert_eq!(bucket_counts.iter().sum::<u64>(), 1);
    assert_eq!(sum, 25.0);
    assert_eq!(count, 1);

    let histogram_attrs = histogram_attributes(&resource_metrics, "chaos.tool_latency");
    let expected_histogram_attributes = BTreeMap::from([
        ("service".to_string(), "chaos-cli".to_string()),
        ("env".to_string(), "prod".to_string()),
        ("tool".to_string(), "shell".to_string()),
    ]);
    assert_eq!(histogram_attrs, expected_histogram_attributes);

    Ok(())
}

// Ensures defaults merge per line and overrides take precedence.
#[test]
fn send_merges_default_tags_per_line() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[
        ("service", "chaos-cli"),
        ("env", "prod"),
        ("region", "us"),
    ])?;

    metrics.counter("chaos.alpha", 1, &[("env", "dev"), ("component", "alpha")])?;
    metrics.counter(
        "chaos.beta",
        2,
        &[("service", "worker"), ("component", "beta")],
    )?;
    metrics.shutdown()?;

    let resource_metrics = latest_metrics(&exporter);
    for (name, expected_value, expected_attrs) in [
        (
            "chaos.alpha",
            1,
            BTreeMap::from([
                ("component".to_string(), "alpha".to_string()),
                ("env".to_string(), "dev".to_string()),
                ("region".to_string(), "us".to_string()),
                ("service".to_string(), "chaos-cli".to_string()),
            ]),
        ),
        (
            "chaos.beta",
            2,
            BTreeMap::from([
                ("component".to_string(), "beta".to_string()),
                ("env".to_string(), "prod".to_string()),
                ("region".to_string(), "us".to_string()),
                ("service".to_string(), "worker".to_string()),
            ]),
        ),
    ] {
        assert_eq!(counter_value(&resource_metrics, name), expected_value);
        assert_eq!(counter_attributes(&resource_metrics, name), expected_attrs);
    }

    Ok(())
}

// Verifies enqueued metrics are delivered by the background worker.
#[test]
fn client_sends_enqueued_metric() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;

    metrics.counter("chaos.turns", 1, &[("model", "gpt-5.1")])?;
    metrics.shutdown()?;

    let resource_metrics = latest_metrics(&exporter);
    assert_eq!(counter_value(&resource_metrics, "chaos.turns"), 1);
    let attrs = counter_attributes(&resource_metrics, "chaos.turns");
    assert_eq!(attrs.get("model").map(String::as_str), Some("gpt-5.1"));

    Ok(())
}

// Ensures shutdown flushes successfully with in-memory exporters.
#[test]
fn shutdown_flushes_in_memory_exporter() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;

    metrics.counter("chaos.turns", 1, &[])?;
    metrics.shutdown()?;

    let resource_metrics = latest_metrics(&exporter);
    assert_eq!(counter_value(&resource_metrics, "chaos.turns"), 1);

    Ok(())
}

// Ensures shutting down without recording metrics does not export anything.
#[test]
fn shutdown_without_metrics_exports_nothing() -> Result<()> {
    let (metrics, exporter) = build_metrics_with_defaults(&[])?;

    metrics.shutdown()?;

    let finished = exporter.get_finished_metrics().unwrap();
    assert!(finished.is_empty(), "expected no metrics exported");
    Ok(())
}
