# chaos-client

Generic transport layer that wraps HTTP requests, retries, and streaming primitives without any Chaos/OpenAI awareness.

- Defines `HttpTransport` and a default `RamaTransport` plus thin `Request`/`Response` types.
- Provides `ChaosHttpClient` request builders; `timeout` covers sending and reading the body, and `execute` buffers the response and reports HTTP failures as `TransportError`.
- Provides retry utilities (`RetryPolicy`, `RetryOn`, `run_with_retry`) for unary and streaming calls, including request builders. `max_attempts` counts retries after the initial request; zero sends once.
- Supplies the `sse_stream` helper to turn byte streams into raw SSE `data:` frames with idle timeouts and surfaced stream errors.
- Consumed by higher-level crates like `chaos-parrot`; it stays neutral on endpoints, headers, or API-specific error shapes.
