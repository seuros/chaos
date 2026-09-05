# chaos-snitch

Telemetry, observability, and audit trail. Structured events, local-first metrics, token usage tracking, and session diagnostics. Opt-in remote reporting only.

Rama OTLP HTTP-client construction and `MetricsClient::start_timer` are
infallible. Exporter setup, network requests, metric/tag validation, snapshots,
and shutdown remain fallible. Global and session timer helpers also remain
fallible because a global exporter may be unavailable and session metadata tags
may be invalid.
