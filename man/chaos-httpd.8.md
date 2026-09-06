# chaos-httpd(8)

## NAME

chaos-httpd - run FreeChaOS as an HTTP trigger server

## SYNOPSIS

```text
chaos serve --bearer-token <TOKEN> [OPTIONS]
```

## DESCRIPTION

HTTP trigger server. Turns Chaos into a webhook target.

One request -> one process -> one response. No sessions, no streaming.

Requires a bearer token. Set via `--bearer-token` or `CHAOS_BEARER_TOKEN` env var.

## OPTIONS

| Flag | Default | Notes |
|------|---------|-------|
| `--bind` | `127.0.0.1` | IPv4 or IPv6 (`::1`, `0.0.0.0`) |
| `--port` | `4000` | |
| `--bearer-token` | env `CHAOS_BEARER_TOKEN` | Required. Empty = rejected at startup. |
| `--timeout` | `600` | Wall-clock seconds per trigger (body reading + start + execution). |
| `--max-concurrent` | `4` | Bounds body reading through process cleanup. Excess requests get `429` before body parsing. |
| `--body-limit` | `1048576` | Bytes. Enforced while streaming, including chunked bodies; oversized `Content-Length` is rejected before reading. |
| `-m, --model` | config default | Server-wide. Per-request override is rejected. |
| `--sandbox` | config default | Sandbox policy for spawned commands. |
| `--skip-git-repo-check` | `false` | |
| `--ephemeral` | `false` | No session persistence. |
| `-C, --cd` | cwd | Working directory for triggered processes. |

Root-level flags (`--provider`, `-c key=value`, etc.) work as with other subcommands.

## ENDPOINTS

### `GET /api/health`

No auth. Returns `200` after startup validation completes.

```json
{"status": "ok", "version": "47.0.0"}
```

### `POST /api/trigger`

```
Authorization: Bearer <token>
Content-Type: application/json
```

#### Request

```json
{
  "request": "Review the latest PR and post feedback",
  "caller_session_id": "optional",
  "conversation_id": "optional",
  "requested_by": "user@example.com",
  "metadata": {}
}
```

- `request` — required, non-empty. Alias: `prompt`.
- `caller_session_id` — correlation field, echoed back. Alias: `session_id`.
- `conversation_id` — auto-generated UUID if omitted. Always returned.
- `requested_by` — recorded in tracing spans.
- `metadata` — opaque JSON, recorded in spans.
- `model` — rejected with `400` if present.

#### Response (200)

```json
{
  "status": "ok",
  "caller_session_id": "...",
  "conversation_id": "...",
  "process_id": "uuid",
  "result": "Agent output text",
  "usage": {
    "total_token_usage": { "input_tokens": 1200, "cached_input_tokens": 300, "output_tokens": 450, "reasoning_output_tokens": 0, "total_tokens": 1650 },
    "last_token_usage": { "input_tokens": 1200, "cached_input_tokens": 300, "output_tokens": 450, "reasoning_output_tokens": 0, "total_tokens": 1650 },
    "model_context_window": 200000
  }
}
```

`usage` is `null` when the provider doesn't report token counts.

#### Errors

All errors are JSON. `caller_session_id` and `conversation_id` are included when available.
`process_id` is included when a process was started.

| Status | Condition |
|--------|-----------|
| `400` | Bad JSON, empty request, unsupported `model` field, wrong `Content-Type` |
| `401` | Missing/wrong bearer token. Includes `WWW-Authenticate: Bearer`. |
| `405` | Wrong method on known route. Includes `Allow` header. |
| `408` | Body-read deadline exceeded; no process was started. |
| `413` | Body exceeds `--body-limit` |
| `429` | Concurrency limit hit |
| `500` | Process error (agent failure, runtime crash). Internal details are logged, not returned. |
| `504` | Timeout exceeded. Process is cleaned up. |

### `GET /monitor` and `GET /monitor/events`

`/monitor` serves a public static connection form, not live server state. Enter
the server bearer token to subscribe to `/monitor/events`. The event endpoint
requires the same `Authorization: Bearer <token>` header as triggers; URL tokens
and cookies are not accepted. The page does not persist the token in browser
storage or send it in query parameters. Reload the page to disconnect.

The authenticated SSE stream includes process/conversation IDs and diagnostic
details. Neither the page nor live responses may be cached. Treat the token as
a full-access credential: it also permits triggering agent runs.

## ARCHITECTURE

Runs in-process via `ProcessTable::start_process` — same runtime path as `chaos exec`, no subprocess.

```
POST /api/trigger
  → auth → content-type → Content-Length preflight → acquire semaphore permit
  → timeout_at(deadline) { bounded body read → deserialize → validate
                          → start process → submit prompt → drain events }
  → cleanup process (bounded 30s grace, always removes from ProcessTable)
  → respond
```

The process lifecycle is split: `runner::start` creates the process handle, `runner::execute` submits and drains events, `runner::cleanup` shuts down and removes from the table. The API layer owns the handle across all three phases, so timeout cancellation always cleans up.

Headless mode (`ApprovalPolicy::Headless`) does not prompt for approval.
Command rules and sandbox policy still apply; commands requiring approval
cannot obtain it through HTTP.

## DEPLOYMENT

Expected behind a reverse proxy (nginx, Caddy, k8s ingress). No TLS, no rate limiting beyond the semaphore.
Use TLS for non-loopback access, forward `Authorization` to the SSE endpoint,
disable proxy buffering/caching for it, and do not log authorization headers.
Apply proxy connection/header timeouts and rate limits as well: the trigger
semaphore is not a limit on idle connections or monitor subscriptions.

## FILES

- `CHAOS_BEARER_TOKEN` - bearer token environment variable
- `~/.chaos/log/` - runtime logs for diagnosing server failures

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-install.7](./chaos-install.7.md)
