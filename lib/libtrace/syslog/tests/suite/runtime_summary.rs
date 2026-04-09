use chaos_ipc::ProcessId;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_syslog::RuntimeMetricTotals;
use chaos_syslog::RuntimeMetricsSummary;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::MetricsClient;
use chaos_syslog::metrics::MetricsConfig;
use chaos_syslog::metrics::Result;
use rama::http::sse::Event as StreamEvent;
use rama::telemetry::opentelemetry::sdk::metrics::InMemoryMetricExporter;
use std::time::Duration;

#[test]
fn runtime_metrics_summary_collects_tool_api_and_streaming_metrics() -> Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let metrics = MetricsClient::new(
        MetricsConfig::in_memory("test", "chaos-cli", CHAOS_VERSION, exporter)
            .with_runtime_reader(),
    )?;
    let manager = SessionTelemetry::new(
        ProcessId::new(),
        "gpt-5.1",
        "gpt-5.1",
        Some("account-id".to_string()),
        None,
        Some(TelemetryAuthMode::ApiKey),
        "test_originator".to_string(),
        true,
        "tty".to_string(),
        SessionSource::Cli,
    )
    .with_metrics(metrics);

    manager.reset_runtime_metrics();

    manager.tool_result_with_tags(
        "shell",
        "call-1",
        "{\"cmd\":\"echo\"}",
        Duration::from_millis(250),
        true,
        "ok",
        &[],
        None,
        None,
    );
    manager.record_api_request(
        1,
        Some(200),
        None,
        Duration::from_millis(300),
        false,
        None,
        false,
        None,
        None,
        "/responses",
        None,
        None,
        None,
        None,
    );
    let sse_response: std::result::Result<
        Option<std::result::Result<StreamEvent, rama::error::BoxError>>,
        tokio::time::error::Elapsed,
    > = Ok(Some(Ok(StreamEvent::default()
        .try_with_event("response.created")
        .expect("valid event")
        .with_data("{}".to_string()))));
    manager.log_sse_event(&sse_response, Duration::from_millis(120));
    manager.record_duration(
        "chaos.turn.ttft.duration_ms",
        Duration::from_millis(95),
        &[],
    );
    manager.record_duration(
        "chaos.turn.ttfm.duration_ms",
        Duration::from_millis(180),
        &[],
    );

    let summary = manager
        .runtime_metrics_summary()
        .expect("runtime metrics summary should be available");
    let expected = RuntimeMetricsSummary {
        tool_calls: RuntimeMetricTotals {
            count: 1,
            duration_ms: 250,
        },
        api_calls: RuntimeMetricTotals {
            count: 1,
            duration_ms: 300,
        },
        streaming_events: RuntimeMetricTotals {
            count: 1,
            duration_ms: 120,
        },
        turn_ttft_ms: 95,
        turn_ttfm_ms: 180,
        ..RuntimeMetricsSummary::default()
    };
    assert_eq!(summary, expected);

    Ok(())
}
