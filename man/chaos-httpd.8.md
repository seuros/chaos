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
| `--timeout` | `600` | Wall-clock seconds per trigger (covers start + execution). |
| `--max-concurrent` | `4` | Semaphore-based. Excess requests get `429` immediately. |
| `--body-limit` | `1048576` | Bytes. Pre-checked via `Content-Length`, enforced post-read. |
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
| `413` | Body exceeds `--body-limit` |
| `429` | Concurrency limit hit |
| `500` | Process error (agent failure, runtime crash). Internal details are logged, not returned. |
| `504` | Timeout exceeded. Process is cleaned up. |

## ARCHITECTURE

Runs in-process via `ProcessTable::start_process` — same runtime path as `chaos exec`, no subprocess.

```
POST /api/trigger
  → auth → content-type → body limit → deserialize → validate
  → acquire semaphore permit
  → timeout_at(deadline) { start process → submit prompt → drain events }
  → cleanup process (bounded 30s grace, always removes from ProcessTable)
  → respond
```

The process lifecycle is split: `runner::start` creates the process handle, `runner::execute` submits and drains events, `runner::cleanup` shuts down and removes from the table. The API layer owns the handle across all three phases, so timeout cancellation always cleans up.

Headless mode (`ApprovalPolicy::Headless`) auto-approves tool use. Interactive events (approval requests, elicitations) are logged as warnings and skipped.

## DEPLOYMENT

Expected behind a reverse proxy (nginx, Caddy, k8s ingress). No TLS, no rate limiting beyond the semaphore.

## FILES

- `CHAOS_BEARER_TOKEN` - bearer token environment variable
- `~/.chaos/log/` - runtime logs for diagnosing server failures

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-install.7](./chaos-install.7.md)
