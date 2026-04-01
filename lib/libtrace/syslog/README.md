# chaos-syslog

`chaos-syslog` is the OpenTelemetry integration crate for Chaos. It provides:

- Provider wiring for log/trace/metric exporters (`chaos_syslog::OtelProvider`
  and `chaos_syslog::provider`).
- Session-scoped business event emission via `chaos_syslog::SessionTelemetry`.
- Low-level metrics APIs via `chaos_syslog::metrics`.
- Trace-context helpers via `chaos_syslog::trace_context` and crate-root re-exports.

## Tracing and logs

Create an OTEL provider from `OtelSettings`. The provider also configures
metrics (when enabled), then attach its layers to your `tracing_subscriber`
registry:

```rust
use chaos_syslog::config::OtelExporter;
use chaos_syslog::config::OtelHttpProtocol;
use chaos_syslog::config::OtelSettings;
use chaos_syslog::OtelProvider;
use tracing_subscriber::prelude::*;

let settings = OtelSettings {
    environment: "dev".to_string(),
    service_name: "chaos-cli".to_string(),
    service_version: env!("CARGO_PKG_VERSION").to_string(),
    chaos_home: std::path::PathBuf::from("/tmp"),
    exporter: OtelExporter::OtlpHttp {
        endpoint: "https://otlp.example.com".to_string(),
        headers: std::collections::HashMap::new(),
        protocol: OtelHttpProtocol::Binary,
        tls: None,
    },
    trace_exporter: OtelExporter::OtlpHttp {
        endpoint: "https://otlp.example.com".to_string(),
        headers: std::collections::HashMap::new(),
        protocol: OtelHttpProtocol::Binary,
        tls: None,
    },
    metrics_exporter: OtelExporter::None,
};

if let Some(provider) = OtelProvider::from(&settings)? {
    let registry = tracing_subscriber::registry()
        .with(provider.logger_layer())
        .with(provider.tracing_layer());
    registry.init();
}
```

## SessionTelemetry (events)

`SessionTelemetry` adds consistent metadata to tracing events and helps record
Chaos-specific session events. Rich session/business events should go through
`SessionTelemetry`; subsystem-owned audit events can stay with the owning subsystem.

```rust
use chaos_syslog::SessionTelemetry;

let manager = SessionTelemetry::new(
    conversation_id,
    model,
    slug,
    account_id,
    account_email,
    auth_mode,
    originator,
    log_user_prompts,
    terminal_type,
    session_source,
);

manager.user_prompt(&prompt_items);
```

## Metrics (OTLP or in-memory)

Modes:

- OTLP: exports metrics via the OpenTelemetry OTLP exporter (HTTP or gRPC).
- In-memory: records via `rama::telemetry::opentelemetry::sdk::metrics::InMemoryMetricExporter` for tests/assertions; call `shutdown()` to flush.

`chaos-syslog` also provides `OtelExporter::Statsig`, a shorthand for exporting OTLP/HTTP JSON metrics
to Statsig using Chaos-internal defaults.

Statsig ingestion (OTLP/HTTP JSON) example:

```rust
use chaos_syslog::config::{OtelExporter, OtelHttpProtocol};

let metrics = MetricsClient::new(MetricsConfig::otlp(
    "dev",
    "chaos-cli",
    env!("CARGO_PKG_VERSION"),
    OtelExporter::OtlpHttp {
        endpoint: "https://api.statsig.com/otlp".to_string(),
        headers: std::collections::HashMap::from([(
            "statsig-api-key".to_string(),
            std::env::var("STATSIG_SERVER_SDK_SECRET")?,
        )]),
        protocol: OtelHttpProtocol::Json,
        tls: None,
    },
))?;

metrics.counter("chaos.session_started", 1, &[("source", "tui")])?;
metrics.histogram("chaos.request_latency", 83, &[("route", "chat")])?;
```

In-memory (tests):

```rust
let exporter = InMemoryMetricExporter::default();
let metrics = MetricsClient::new(MetricsConfig::in_memory(
    "test",
    "chaos-cli",
    env!("CARGO_PKG_VERSION"),
    exporter.clone(),
))?;
metrics.counter("chaos.turns", 1, &[("model", "gpt-5.1")])?;
metrics.shutdown()?; // flushes in-memory exporter
```

## Trace context

Trace propagation helpers remain separate from the session event emitter:

```rust
use chaos_syslog::current_span_w3c_trace_context;
use chaos_syslog::set_parent_from_w3c_trace_context;
```

## Shutdown

- `OtelProvider::shutdown()` stops the OTEL exporter.
- `SessionTelemetry::shutdown_metrics()` flushes and shuts down the metrics provider.

Both are optional because drop performs best-effort shutdown, but calling them
explicitly gives deterministic flushing (or a shutdown error if flushing does
not complete in time).
