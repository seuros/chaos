# Application tracing through Snitch

Higher-level application crates use the existing `chaos-snitch` crate's
`tracing` module. Its implementation is centralized in
`lib/libtrace/snitch/src/tracing.rs`, which re-exports Rama's supported API:

| API | Import |
| --- | --- |
| Events, spans, traits, types | `chaos_snitch::tracing` |
| Subscriber registry, layers, filters, formatting | `chaos_snitch::tracing::subscriber` |
| Non-blocking writers and rolling files | `chaos_snitch::tracing::appender` |
| OpenTelemetry layer and span extensions | `chaos_snitch::tracing::{layer, OpenTelemetrySpanExt}` |

Use `chaos-snitch = { workspace = true }` in application consumers.
Lower-level libraries keep `rama = { workspace = true }` and import
`rama::telemetry::tracing` directly. In particular, Snitch depends on IPC,
the HTTP client, process state, packet filtering, and configuration:
making these depend back on Snitch would create cycles. Do not introduce
a second facade crate or force lower-level helpers to depend on Snitch.
The independently publishable MCP guest also stays self-contained; it now
depends on Rama even without its HTTP feature.
Rama already enables the subscriber features we use.
Aliases such as `subscriber as tracing_subscriber` keep existing qualified
references readable; they are not separate dependencies.

Rama customizes `span!`, `trace_span!`, `debug_span!`, and `info_span!`.
With OpenTelemetry enabled, these also record `span.id` and `trace.id`.
Rama's `subscriber` re-export names the **subscriber crate**, not the original
`tracing::subscriber` module. For scoped defaults, use
`tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), f)`
or the subscriber crate's `SubscriberInitExt::set_default`.

## Remaining direct dependencies

These are intentional exceptions for Rama 0.4.0, not a completed removal:

| Crates | Reason | Removal prerequisite |
| --- | --- | --- |
| `chaos-recall`, `chaos-parrot`, `chaos-mcp-runtime` | `#[instrument]` generates absolute `::tracing` paths | An instrumentation macro that supports the re-export path |
| `chaos-fork`, `chaos-kern`, `chaos-console` | Explicit `tracing/log`; also instrumentation attributes | Rama feature forwarding for `tracing/log` and instrumentation compatibility |
| `chaos-mcpd`, `libui`, `chaos-termcap` | Explicit `tracing/log` | Rama feature forwarding for `tracing/log` |
| `chaos-pf` (dev-dependency only) | Custom test subscriber implements `register_callsite`, whose return type is `tracing::subscriber::Interest` | Stable Rama re-export of the original subscriber module or `Interest` |

Keep these requirements explicit rather than relying on another workspace
member to enable a feature. Do not use private tracing/Rama internals or a local
compatibility facade to bypass them.

`tracing-test` and `opentelemetry-appender-tracing` remain separate dependencies:
Rama does not replace them here. Tracing packages are still expected as
transitive dependencies in the root lockfile.

## Independent drivers

Driver workspaces maintain their own lockfiles and tracing APIs:

- Dictator uses `dictator_core::tracing`, backed by Rama's minimal `std` feature.
- Skipper uses its local `crate::tracing` module, backed by its existing Rama dependency.
- Helmsman has no direct tracing dependency to replace.

These workspaces are validated and released independently of the application.
Changes to Rama itself are deferred.
