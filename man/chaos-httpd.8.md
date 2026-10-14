+++
title = "chaos-httpd(8)"
summary = "HTTP daemon configuration and operation."
+++

# chaos-httpd(8)

## NAME

chaos-httpd - run FreeChaOS as an HTTP trigger server

## SYNOPSIS

```text
chaos serve --bearer-token <TOKEN> [OPTIONS]
```

## DESCRIPTION

HTTP trigger server. Turns Chaos into a webhook target.

By default, one request -> one process -> one response.
`--sessions` additionally enables an experimental daemon-owned session API,
with asynchronous turns, live steering and authenticated event replay/SSE.

Requires a bearer token. Set via `--bearer-token` or `CHAOS_BEARER_TOKEN` env var.

## OPTIONS

| Flag | Default | Notes |
|------|---------|-------|
| `--bind` | `127.0.0.1` | IPv4 or IPv6 (`::1`, `0.0.0.0`) |
| `--port` | `4000` | |
| `--bearer-token` | env `CHAOS_BEARER_TOKEN` | Required. Empty = rejected at startup. |
| `--timeout` | `600` | Wall-clock seconds per trigger (body reading + start + execution). |
| `--max-concurrent` | `4` | Bounds triggers from body reading through cleanup, and active retained turns through background quiescence. |
| `--body-limit` | `1048576` | Bytes. Enforced while streaming, including chunked bodies; oversized `Content-Length` is rejected before reading. |
| `--sessions` | `false` | Enable the experimental retained-session endpoints. |
| `--max-sessions` | `32` | Maximum retained records, including closed-session tombstones. |
| `--session-ttl` | `3600` | Maximum session lifetime in seconds, including active execution. |
| `-m, --model` | config default | Server default. `/api/trigger` still rejects per-request overrides. |
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

### Experimental retained sessions (`--sessions`)

This is a validation API, **not yet a production replacement for souls.house's
durable execution ledger**. Provider, credentials, working directory, tool
environment and sandbox are server-owned. Clamp transports are rejected at
startup when this API is enabled. Per-turn environment isolation and invocation
billing telemetry are not implemented. Host notices (below) expire and can be
withdrawn, but reach the model only at native sample boundaries until clamp is
supported here.

```sh
chaos serve --sessions -C /path/to/project
```

All session endpoints require the same bearer authentication as `/api/trigger`.
Responses have `Cache-Control: no-store`. Mutations additionally require
`X-Chaos-Daemon-ID`, obtained from `GET /api/sessions`. A missing generation
returns `428`; a changed generation returns `409`. **Never simply refresh this
header and replay an uncertain submission.** Reconcile it through your own
durable ledger first.

| Method | Path | Behavior |
|--------|------|----------|
| GET | `/api/sessions` | Current daemon generation and bounded session list |
| POST | `/api/sessions` | Accept creation; return `202` and a server-generated session UUID |
| GET | `/api/sessions/{session}` | Read state, process ID and active turn ID |
| POST | `/api/sessions/{session}/turns/{turn}` | Accept a caller-generated turn UUID; return `202` without waiting for inference |
| GET | `/api/sessions/{session}/turns/{turn}` | Read execution state, bounded result and usage snapshot |
| POST | `/api/sessions/{session}/turns/{turn}/steer` | Inject input only into that active foreground turn |
| POST | `/api/sessions/{session}/turns/{turn}/notice` | Replace the host notice for that active foreground turn |
| DELETE | `/api/sessions/{session}/turns/{turn}/notice` | Withdraw the host notice |
| POST | `/api/sessions/{session}/turns/{turn}/interrupt` | Request interruption; `202` is not proof of termination |
| GET | `/api/sessions/{session}/events?after=0` | Bounded, sequence-numbered kernel event replay |
| DELETE | `/api/sessions/{session}` | Request shutdown; delete a closed/failed record on a subsequent call |

Creation accepts `{}` or `{"resume_process_id":"saved-process-uuid"}`.
Poll the session until its status is `idle` before submitting a turn. A process
already running in this daemon cannot be resumed into a second event consumer.
Explicit resume restores saved history, not an in-flight model request, tool or
shell command, and cannot be used with `--ephemeral`.

Turn submission accepts:

```json
{"request":"Run the tests","model":"optional-model-slug","reasoning_effort":"medium"}
```

Only the existing provider is used; model/effort changes apply to the submitted
turn and subsequent session context. No credential, environment, sandbox or
provider overrides are accepted. Repeating a turn UUID with the same payload
returns its existing state without executing again; changing the payload returns
`409`. One HTTP turn may occupy a session at a time. At most 256 turn records
are retained per session; reaching this limit returns `429` rather than silently
discarding deduplication evidence.

Steering accepts `{"request":"Focus on the failing parser test"}`. It returns
`200` on kernel acceptance, not on model consumption. An inactive or mismatched
turn returns `409`. Steering is **not idempotent**: after a lost acknowledgement,
do not automatically retry. Interrupting does not itself terminate background
terminal processes; session shutdown is the stronger operation.

A host notice is for time-sensitive reminders that must not outlive their
moment: `{"id":"n-42","text":"You have not posted for two minutes","expires_at":1760000000}`
(`expires_at` is optional Unix seconds). Unlike steering it is not queued as
input: the session holds one notice, expiry is checked again at the delivery
boundary, and a newly admitted turn clears any undelivered notice. Use a fresh
`id` for every notice: 1–128 characters of `A-Z a-z 0-9 - . _ ~`, so it can be
named in a query string without encoding; anything else is `400`. A POST whose `id` the session has already seen (among
its last 64 posted or delivered ids) is a no-op (`notice_already_seen`), so a
retried or reordered POST can neither re-deliver an old notice nor displace a
newer one. Only the turn that posted the pending notice can withdraw it.
`DELETE …/notice?id=<id>` withdraws only that notice and is safe to retry;
a bare `DELETE` withdraws whatever that turn has pending. `chaos exec
--steer-file` feeds the same slot.

Turn states include `accepted`, `running`, `waiting_background`, `interrupting`,
`completed`, `failed`, `interrupted`, `timed_out` and `unknown`. Execution capacity
is retained until matching completion and kernel background quiescence, or
confirmed session shutdown. An error event or cancellation request alone does
not release it. Failed containment remains `unknown` and occupies capacity;
an explicit DELETE may retry containment. The limit does not count detached
processes, external side effects or all sub-agent work.

Turn results are capped at 16 KiB with `result_truncated`. Usage is the kernel's
cumulative/last-request snapshot, **not invocation-local billing telemetry**.
The turn deadline uses `--timeout`; hitting it shuts down the session instead
of reusing ambiguous partial context. Control requests have their own bounded
ingress and remain available when execution capacity is full.

Request `Accept: text/event-stream` on the events endpoint for SSE. Reconnection
supports `Last-Event-ID` or the `after` query cursor. The stream sends `kernel`,
`session` and explicit `gap` events; disconnect does not cancel execution.
On daemon shutdown, session SSE streams close with `shutting_down: true` even
when containment is uncertain; this does not change an `unknown` execution into
a confirmed terminal outcome.
Replay retains at most 256 events / 1 MiB; events over 64 KiB are replaced with
`http_event_omitted` markers. JSON replay returns `events`, `gap` and
`next_cursor` inside `replay`. A cursor ahead of the session returns `400`.
Never treat an event gap as proof of completion; read turn status.

**These are private runtime events**, potentially containing prompts, tool
arguments/results and reasoning. A frontend adapter must implement its own
authorization, redaction and narration-consent projection. Do not expose this
stream directly to public browsers.

Session records and deduplication are memory-only and scoped to the daemon
generation. History persistence follows normal kernel configuration. On daemon
restart, missing sessions/turns mean **unknown outcome**, not permission to
re-execute. Session TTL closes execution but retains its record; DELETE the
closed record only after durably accounting for its outcomes. Forgetting a
record also forgets its in-memory turn tombstones.

## ARCHITECTURE

Runs in-process via `ProcessTable::start_process` — same runtime path as `chaos exec`, no subprocess.

With `--sessions`, a bounded registry retains process supervisors independently
of HTTP handlers. Each supervisor alone drains its process's kernel event queue;
polling and SSE clients read a separate bounded replay buffer. Commands and
execution have separate admission limits.

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
