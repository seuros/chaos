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
| `--session-scope` | unset | Durable IDs/outcomes in journald under a stable server namespace. Requires `--sessions`, forbids `--ephemeral`. |
| `--recovery-token` | env `CHAOS_RECOVERY_TOKEN` | Enable operator recovery mutations. Requires `--session-scope` and a nonempty token different from the ordinary bearer. |
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

This is a validation API, **not yet a production execution service**.
Provider, credentials, working directory, tool
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
An optional canonical UUID `session_id` makes creation retryable within the
retained namespace; a different requested process for an existing ID conflicts.
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

Turn states include `persisting`, `accepted`, `running`, `waiting_background`, `interrupting`,
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

Without `--session-scope`, records and deduplication are memory-only and scoped to the daemon
generation. History persistence follows normal kernel configuration. On daemon
restart, missing sessions/turns mean **unknown outcome**, not permission to
re-execute. Session TTL closes execution but retains its record; DELETE the
closed record only after durably accounting for its outcomes. Forgetting a
record also forgets its in-memory turn tombstones.

#### Durable retained sessions

```sh
chaos serve --sessions --session-scope project-worker -C /path/to/project
```

Choose a distinct, stable namespace for each tenant/configuration. Reusing one
with changed credentials, provider, working directory, sandbox or tools is not
supported. The namespace is server configuration, never an HTTP override.

Journald commits acceptance before dispatch and terminal outcomes before
execution capacity is released. A disconnected client cannot cancel those
writes. Ownership uses 90-second fenced leases, renewed every 15 seconds;
storage calls have a 5-second deadline. Lost storage/ownership triggers bounded
containment and retains uncertain capacity. No lease token is exposed over HTTP.

Graceful daemon shutdown contains processes and releases idle history. On the
next start, its session UUID, process mapping and saved turn outcomes are restored.
An idle process resumes lazily on a **fresh** turn, never by replaying an old
prompt. Explicit DELETE or session TTL closes the record instead of suspending it.
Closed durable records retain their deduplication tombstones; DELETE cannot erase
them. `--max-sessions` therefore bounds all records in the durable namespace.

Expired ownership with unfinished work, or even a bound idle process that was
not orderly released, recovers as `unknown` and consumes execution capacity.
An interrupted, unbound creation is closed without starting a kernel. Startup
fails if a live owner conflicts, retained records exceed `--max-sessions`, or
uncontained reservations exceed `--max-concurrent`. Wait for lease expiry after
an unclean exit; expiry itself does not prove tools stopped.

Recovered unknown work is never automatically resumed. DELETE cannot establish
containment of a previous daemon's external processes; use operator recovery
after independent containment. Autonomous
kernel work without a tracked HTTP turn triggers containment in this preview.

#### Operator recovery

`GET /api/sessions/<session_id>/recovery` returns persisted state, quarantine
`recovery_epoch`, an immutable resolution audit if present, and a supervisor-stop
hint. It requires the ordinary bearer; hints do not prove external work stopped.

`POST` on the same endpoint requires `--recovery-token` (or `CHAOS_RECOVERY_TOKEN`),
the ordinary bearer, current `X-Chaos-Daemon-ID`, and a separate
`X-Chaos-Recovery-Authorization: Bearer <operator token>`.
The body contains a caller-stable `resolution_id`, `expected_process_id`,
`expected_turn_id` (null when absent), `expected_recovery_epoch`,
`containment_confirmed: true`, and bounded `actor`/`reason` evidence.

Only assert containment after independently stopping the executor and its
external subprocesses/remote tools. A live local supervisor returns `409`: stop
the daemon, contain work, wait for ownership expiry if necessary, restart into
quarantine, then inspect. New quarantines/takeovers invalidate old epochs; do not
automatically refresh a stale assertion. The server records operator assertions,
not proof of containment or verified actor identity. Keep the recovery credential
out of application/tool environments; it is not a sandbox boundary.

The audit, interrupted turn outcome, session closure and lease release commit
atomically before local capacity is released. Failed/ambiguous commits retain
capacity. Read the audit and retry only the identical resolution; changed
decisions return `409`. Disconnect cannot cancel the daemon-owned write.
Resolution closes permanently and preserves deduplication; it never resumes
history or replays input. Continuing requires an explicit new session.

After a generation change, read saved state before refreshing the mutation
header. Same-payload retries return the saved outcome; changed payloads conflict.
Replay/SSE buffers remain memory-only: discard old event cursors after restart,
and use persisted turn state rather than event absence to reconcile.

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
Forward `X-Chaos-Recovery-Authorization` only on trusted recovery routes and
redact it in proxy/access logs.
Apply proxy connection/header timeouts and rate limits as well: the trigger
semaphore is not a limit on idle connections or monitor subscriptions.

## FILES

- `CHAOS_BEARER_TOKEN` - bearer token environment variable
- `~/.chaos/log/` - runtime logs for diagnosing server failures

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-install.7](./chaos-install.7.md)
