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

Builds with `mcpd` (the default) support authenticated MCP at `/mcp` through
`--mcp`. Set permitted browser origins with `--mcp-allowed-origin`.

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
| `--session-recovery-only` | `false` | Drain legacy/changed-policy history without execution. Requires `--session-scope` and `--recovery-token`; every protected route also requires the operator header. |
| `--session-webhook-url` | unset | Deliver durable terminal events to a trusted server-owned HTTPS URL (literal loopback HTTP allowed for development). Requires `--session-scope`. |
| `--session-webhook-secret-env` | `CHAOS_SESSION_WEBHOOK_SECRET` | Variable containing a random webhook HMAC key of at least 32 bytes. Keep it out of all tool environments. |
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

**Experimental, not a production execution service.** Health does not prove
storage availability, containment, or webhook delivery. Provider, credentials,
cwd, environment, and sandbox are server-owned; clamp and per-turn environment
isolation are unsupported. Accounting is partial, not a provider invoice.

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
| GET | `/api/sessions/{session}/turns/{turn}/accounting` | Durable invocation records and partial reported-usage summary |
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

Turn states include `persisting`, `accepted`, `running`, `waiting_background`,
`interrupting`, `completed`, `failed`, `interrupted`, `timed_out`, and `unknown`.
Errors and cancellation requests alone do not release capacity: work must stop.
`unknown` occupies capacity; DELETE may retry containment. Limits do not cover
all detached processes, external effects, or sub-agent work.

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
Scoped replay survives restart, with markers for potentially unobserved work.
Events are observations, not authoritative outcomes. After gaps or disconnects,
read turn status; event absence never proves completion.

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

Choose a distinct stable scope per tenant/configuration. A scope binds provider
and credential identity, cwd/home, permissions, environment, tools, roles, and
instructions. Intentional changes require a new scope, even after records close.
Configuration drift stops work, not authorizes replay. OAuth token rotation is
allowed only for the same identity.
HTTP MCP requires explicit bearer authorization; anonymous access and implicit
MCP OAuth are unsupported here.

Process-history ownership survives closure. Explicit resume requires an existing
matching tenant/policy binding before loading history; arbitrary CLI or legacy
history cannot be imported. Legacy namespaces cannot silently adopt a policy.

Client disconnects do not cancel accepted work or outcome recording.
Storage/ownership loss stops work where possible and retains uncertain capacity.
After an unclean exit, ownership may take up to 90 seconds to expire;
expiry is not proof that tools stopped.

Graceful daemon shutdown contains processes and releases idle history. On the
next start, its session UUID, process mapping and saved turn outcomes are restored.
An idle process resumes lazily on a **fresh** turn, never by replaying an old
prompt. Explicit DELETE or session TTL closes the record instead of suspending it.
Closed durable records retain their deduplication tombstones; DELETE cannot erase
them. `--max-sessions` therefore bounds all records in the durable namespace.

Uncleanly released sessions recover as `unknown` and consume capacity.
Startup fails on live-owner conflicts or retained records/reservations exceeding
configured limits.

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

Failed/ambiguous recovery writes retain capacity. Read the audit and retry only
the identical resolution; changed decisions return `409`.
Resolution closes permanently and preserves deduplication; it never resumes
history or replays input. Continuing requires an explicit new session.

#### Invocation-local accounting

`GET …/turns/{turn}/accounting` returns provider/requested-model/requested-tier
metadata, purpose, storage timestamps, observed response/server-model metadata,
normalized input/cache-write/cache-read/output/reasoning/total counters and
explicit missing/pending/unknown states. No prompts or credentials are included.
It uses normal session authentication and also the operator header in
recovery-only mode. Pre-metering turns return `enabled: false, summary: null`.

Missing/interrupted usage is not zero/free. Failed accounting stops further
dispatch and retains uncertain capacity. At most 128 logical calls are allowed
per turn. Records survive closure and restart.

Repeated nonempty response IDs within a scope/provider retain a
`duplicate_receipt_of` link and are excluded from the reported subtotal, without
asserting that the duplicate was free. Missing IDs cannot support that deduplication.
Overflow or absent unique usage yields a null subtotal, not zero.
Terminal webhooks include the summary; full receipts remain on this endpoint.

**Not a provider bill:** every summary says `billing_complete: false`. New
turns record `coverage: root_and_descendant_logical_sampling_and_compaction`;
old turns retain direct-only provenance. Metered calls do not automatically retry.
Planning/reflex work, root startup/autonomous
calls, catalogue traffic, tools, clamp, memory-only sessions and `/api/trigger`
are outside this coverage. Requested model/tier are not proof of a charged SKU.
Even when recorded receipts have usage, excluded work may incur more cost.
PostgreSQL-only immutable price schedules and receipt imports provide exact integer estimates,
not credit reservations, invoice verification or native receipt overwrites.
A billing adapter must verify provider evidence; missing/unknown/duplicate
data is never a refund. Authorize/redact private receipt metadata for frontends.

#### Recovery-only draining

To drain a legacy scope or one whose original policy is no longer available:

```sh
chaos serve --sessions --session-scope old-worker --session-recovery-only
```

Supply both bearer/recovery credentials through their environment variables.
All protected reads as well as recovery POST require the operator header.
Triggers and execution mutations are rejected.
Operator-only price/evidence and archive operations remain available.
No execution starts or resumes; released history is quarantined for independent
containment. Use the original storage backend/home and sufficient capacity.
Closing old records never permits overwriting their namespace's policy.

#### Durable terminal webhooks

```sh
# Set CHAOS_SESSION_WEBHOOK_SECRET securely in the server environment.
chaos serve --sessions --session-scope project-worker \
  --session-webhook-url https://adapter.example/internal/chaos
```

Terminal outcomes and operator resolutions produce durable events, retained even
without a target. Enabling delivery sends pending events. `unknown` execution
produces no terminal callback; triggers and live SSE are not included.

The destination/signing identity is immutable per scope; changed values fail
startup. Signing keys must differ from bearer/recovery tokens. Omit the URL
to pause, or use recovery-only mode with the original target/key to drain
old events. No per-request callback override is accepted.

POST carries `X-Chaos-Event-ID`, `X-Chaos-Timestamp` (Unix seconds),
`X-Chaos-Delivery-Attempt`, and
`X-Chaos-Signature: v1=<HMAC-SHA256 hex>`. Sign/verify exactly
`timestamp + "." + event_id + "." + raw_body`, in bytes. Receivers must verify
in constant time, bound timestamp skew and match header/body IDs.
The version-1 body identifies scope/session/process, terminal turn/outcome and
optional recovery resolution UUID. No-turn recovery emits `session.resolved`;
terminal turns emit `turn.terminal`. Outcomes are private: project/redact them
before exposing a frontend. Actor/reason evidence and leases are not included.

**At-least-once delivery:** persist/deduplicate the event UUID atomically with
receiver effects, then return 2xx. A lost HTTP/storage acknowledgement may
deliver the same immutable bytes again, possibly concurrently or out of order.
Delivery failure is never permission to replay execution.

Delivery has a 10-second HTTP timeout and retries from 2 seconds to 1 hour.
Redirects are not followed; only 2xx acknowledges delivery. Failures/crashes
retain pending events, and delivery does not occupy execution capacity.

`GET /api/sessions/deliveries[?after=<event UUID>]` lists up to 256 UUID-sorted
status records (attempts/deadlines/delivery timestamps), without bodies or leases.
This is metadata pagination, not an append-only event cursor. Refresh the list
for new events. Authentication follows other session reads, including the
operator header in recovery-only mode.

#### Operator financial evidence and archival

These routes require the ordinary bearer and separate operator header, including
GET. Mutations also require the current daemon generation:

| Method | Path | Behavior |
|--------|------|----------|
| GET, PUT | `/api/sessions/prices/{version}` | Immutable versioned exact-integer token prices |
| GET, POST | `/api/sessions/{session}/turns/{turn}/billing` | Immutable external provider evidence, separate from native receipts |
| GET, POST | `/api/sessions/{session}/archive` | Read/export a closed-session manifest or seal its verified digest |

Price and billing routes exist only when the retained-session store uses
PostgreSQL. On SQLite those paths return `404`.
Raw invocation receipts and execution archival remain available on both backends.
SQLite archives omit financial evidence sections.
Upgrading SQLite with legacy financial evidence requires explicit export or
migration to PostgreSQL; populated financial tables are not automatically deleted.

Archival is non-destructive: original outcomes, receipts and
tombstones remain, and no session-cap capacity is reclaimed.

## DEPLOYMENT

Run behind a reverse proxy for TLS and rate limiting.
Commands requiring interactive approval cannot obtain it through HTTP;
command rules and sandbox policy still apply.
Use TLS for non-loopback access, forward `Authorization` to the SSE endpoint,
disable proxy buffering/caching for it, and do not log authorization headers.
Forward `X-Chaos-Recovery-Authorization` only on trusted recovery routes and
redact it in proxy/access logs.
Apply proxy connection/header timeouts and rate limits as well: the trigger
concurrency limit does not bound idle connections or monitor subscriptions.

Scope bindings are control-plane protections, **not an OS tenant sandbox**.
Fingerprints do not freeze executable tools, arbitrary files or remote server
implementations, nor fence external effects already started. Mutually untrusted
tenants need separate OS users/containers, restricted mounts, separate homes and
credential stores, and restrictive VFS/environment/network policies. The ordinary
bearer still grants powerful server-owned tools.

Webhook I/O is daemon control-plane traffic, outside the model tool sandbox.
Restrict daemon egress and callback destinations. Startup rejects signing-variable
exposure through the base shell policy or explicit MCP environment selections;
also exclude it from role overrides, shell profiles, hooks and other tool
environments. No model credentials or infrastructure cookies accompany callbacks.

## FILES

- `CHAOS_BEARER_TOKEN` - bearer token environment variable
- `CHAOS_SESSION_WEBHOOK_SECRET` - default webhook signing-key variable
- `~/.chaos/log/` - runtime logs for diagnosing server failures

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-install.7](./chaos-install.7.md)
