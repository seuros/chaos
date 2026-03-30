use chaos_ipc::ProcessId;
use chaos_ipc::protocol::SessionSource;
use chaos_syslog::RuntimeMetricTotals;
use chaos_syslog::RuntimeMetricsSummary;
use chaos_syslog::SessionTelemetry;
use chaos_syslog::TelemetryAuthMode;
use chaos_syslog::metrics::MetricsClient;
use chaos_syslog::metrics::MetricsConfig;
use chaos_syslog::metrics::Result;
use eventsource_stream::Event as StreamEvent;
use pretty_assertions::assert_eq;
use rama::error::BoxError;
use rama::http::ws::Message;
use rama::telemetry::opentelemetry::sdk::metrics::InMemoryMetricExporter;
use std::time::Duration;

#[test]
fn runtime_metrics_summary_collects_tool_api_and_streaming_metrics() -> Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let metrics = MetricsClient::new(
        MetricsConfig::in_memory("test", "codex-cli", env!("CARGO_PKG_VERSION"), exporter)
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
    manager.record_websocket_request(Duration::from_millis(400), None, false);
    let sse_response: std::result::Result<
        Option<std::result::Result<StreamEvent, eventsource_stream::EventStreamError<&str>>>,
        tokio::time::error::Elapsed,
    > = Ok(Some(Ok(StreamEvent {
        event: "response.created".to_string(),
        data: "{}".to_string(),
        id: String::new(),
        retry: None,
    })));
    manager.log_sse_event(&sse_response, Duration::from_millis(120));
    let ws_response: std::result::Result<
        Option<std::result::Result<Message, BoxError>>,
        chaos_parrot::ApiError,
    > = Ok(Some(Ok(Message::Text(
        r#"{"type":"response.created"}"#.into(),
    ))));
    manager.record_websocket_event(&ws_response, Duration::from_millis(80));
    let ws_timing_response: std::result::Result<
        Option<std::result::Result<Message, BoxError>>,
        chaos_parrot::ApiError,
    > = Ok(Some(Ok(Message::Text(
        r#"{"type":"responsesapi.websocket_timing","timing_metrics":{"responses_duration_excl_engine_and_client_tool_time_ms":124,"engine_service_total_ms":457,"engine_iapi_ttft_total_ms":211,"engine_service_ttft_total_ms":233,"engine_iapi_tbt_across_engine_calls_ms":377,"engine_service_tbt_across_engine_calls_ms":399}}"#
            .into(),
    ))));
    manager.record_websocket_event(&ws_timing_response, Duration::from_millis(20));
    manager.record_duration(
        "codex.turn.ttft.duration_ms",
        Duration::from_millis(95),
        &[],
    );
    manager.record_duration(
        "codex.turn.ttfm.duration_ms",
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
        websocket_calls: RuntimeMetricTotals {
            count: 1,
            duration_ms: 400,
        },
        websocket_events: RuntimeMetricTotals {
            count: 2,
            duration_ms: 100,
        },
        responses_api_overhead_ms: 124,
        responses_api_inference_time_ms: 457,
        responses_api_engine_iapi_ttft_ms: 211,
        responses_api_engine_service_ttft_ms: 233,
        responses_api_engine_iapi_tbt_ms: 377,
        responses_api_engine_service_tbt_ms: 399,
        turn_ttft_ms: 95,
        turn_ttfm_ms: 180,
    };
    assert_eq!(summary, expected);

    Ok(())
}
