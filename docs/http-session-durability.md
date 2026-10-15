# HTTP session durability

The experimental `chaos serve --sessions --session-scope <namespace>` API uses
journald for admission, dispatch identity, terminal outcomes and ownership.
Without a scope the existing memory-only behavior is unchanged. Keep the daemon
generation precondition: after restart, read saved state before retrying any turn.

## Storage contract

`JournalClient::execution` exposes the same typed control-plane operations over
the SQLite journald Unix socket and the direct PostgreSQL backend. Migration 26
adds session and turn tables to the existing runtime database. Migration 27 adds
immutable operator resolutions and monotonic quarantine epochs. Migration 28
adds immutable scope-policy bindings and permanent process-history ownership.
Migration 29 adds the terminal-event outbox and destination bindings.
Migration 30 adds bounded invocation records and explicit metering provenance.
Migration 31 persists bounded observation replay; 32 adds archive manifests;
33 adds immutable price/evidence records; 34 adds fenced descendant lineage and
per-turn accounting coverage provenance.
Journald protocol 12 prevents accidentally talking to an older sidecar.
Conversation transcripts remain in the existing append-only journal.

A server selects a stable configuration/tenant `scope` and a unique owner ID for
each daemon incarnation. Neither value is permission to switch provider,
credentials, billing, tools, cwd or sandbox. Applications must not pass lease
tokens through the public HTTP API.

1. **BindScope** pins a policy digest to an empty namespace; **ClaimBound**
   obtains a fenced lease only under that policy. Legacy claims cannot bypass
   a bound namespace. A namespace with legacy records cannot adopt a policy.
2. **BindNewProcess** registers a server-generated fresh process ID.
   **BindProcess** accepts existing history only with matching scope and policy,
   before loading it into a kernel. Ownership survives session closure.
   Live and unknown sessions cannot map to the same process.
   A quarantined startup without an active turn may bind its newly allocated ID;
   it remains non-executable until independently reconciled idle.
3. **Accept** atomically stores a turn UUID, normalized SHA-256 fingerprint and
   active reservation. Same-payload retries return the original record;
   changed-payload retries conflict. No prompt is stored in the ledger.
4. **Dispatch** atomically changes accepted to running. Only
   `newly_dispatched: true` authorizes the initial kernel submission. A retry
   returning false is **not** permission to submit again. Losing the response
   requires reconciliation, not another model/tool invocation.
5. **Finish** persists a bounded terminal outcome and clears the active
   reservation in one transaction. The caller must first establish quiescence
   or containment. A failed/ambiguous commit must not release local capacity.
   Same terminal retries are idempotent; changing a terminal outcome conflicts.
6. Independently contained idle/closed owners may **Release** their lease.
   Unfinished/unknown sessions cannot release ownership or close. Closed records
   retain their deduplication history.
7. **Resolve** atomically records an operator's containment assertion, interrupts
   an active unknown turn, closes the session and releases its lease. The
   expected process, turn and quarantine epoch must still match under the writer
   lock. Changed resolutions conflict; identical retries only read the committed
   audit, including after lease expiry/release. No execution is authorized.

Leases use the storage clock and are validated while holding the session writer
lock. SQLite uses `BEGIN IMMEDIATE`; PostgreSQL uses `FOR UPDATE`. A live claim
retry by the same owner returns the original token without extending its
deadline. Heartbeats require the current unexpired token; caller-provided
deadlines are never trusted. Lease TTLs are bounded to 1–300 seconds.

## Owner loss

Acquiring an expired lease atomically fences the previous writer and marks its
unfinished turn **unknown**, even when only acceptance was committed.
An expired idle owner with a bound process also becomes unknown: an idle HTTP
turn ledger is not proof that autonomous kernel work stopped. Orderly released
idle history preserves its idle state on handoff.

- Unknown is not terminal and retains the active reservation and process mapping.
- An unknown turn cannot dispatch; another turn cannot be admitted.
- Lease expiry is not evidence that subprocesses or remote tools stopped.
- Reading records and taking ownership do not resume a process or replay work.
- The new owner may persist a terminal resolution only after independently
  establishing the outcome/containment. Storage cannot verify that evidence.

**Quarantine** can mark the whole process unknown even without an HTTP turn.
After independent containment, **ReconcileIdle** may unblock that no-turn case.
An active unknown turn must instead receive an explicit terminal **Finish**.

The ledger fences storage writes, **not external side effects** already launched
by a previous owner. The HTTP supervisor stops on ownership/storage loss,
retains its capacity reservation, and performs bounded containment. It must not
treat a rejected heartbeat as proof of cancellation.

## HTTP recovery and bounds

Creation and admission run in daemon tasks that retain their ingress reservations
even when HTTP disconnects. New kernels are quarantined before boot, so crashing
between boot and recording a process ID cannot produce apparently healthy idle
work. The ledger reports the persisted turn count, so unloaded tombstones cannot
bypass the HTTP admission cap.

Leases are renewed independently every 15 seconds with a 90-second TTL; storage
calls time out after 5 seconds. Dispatch requires the unique successful
`newly_dispatched` transition. Terminal event processing waits for kernel
quiescence, commits its outcome, then releases the local execution permit.
Native quiescence checks include descendant processes, queued submissions and
pending descendant boots, not just the root's completion event. An abandoned
boot remains uncertain. Orderly shutdown stops the root, contains tracked
descendants, and only then permits durable handoff; global registry cleanup
must not erase that evidence first.

Graceful shutdown contains the backend and releases idle history; recovery keeps
its stable session ID and lazily resumes the process on a fresh accepted turn.
Expired ownership is quarantined, including idle processes not orderly released.
Recovered unknown records have no executor and retain capacity until independent
containment/reconciliation. An unbound idle creation is closed without boot.
Explicit DELETE and TTL expiration close records, not the namespace's history.

Each session keeps at most 256 turn tombstones. Outcomes are bounded to 32 KiB
of serialized JSON; list pages to 256 sessions. There is no implicit tombstone
eviction or deletion that could make an old turn UUID executable again.

Closed records are not forgotten in durable mode. The namespace-wide session cap
includes them; startup refuses live ownership conflicts or uncontained work that
cannot fit the configured capacity. Session lifetime restarts with each daemon
incarnation, not a persisted wall-clock deadline.

## Scope-policy isolation

Each daemon serves one server-selected tenant/configuration namespace. Its
immutable digest binds provider configuration, pinned credential identity,
working directory, home, effective permissions/environment policy, configured
tools, role material and instructions. A different policy fails before claiming
history, even if every session is closed. Use a new namespace for legitimate
policy or credential changes; there is no policy-overwrite endpoint.

Credential snapshots cover configured providers, including delegated roles.
API keys and configured credential-source values are hashed in private memory;
only the digest is persisted, never the raw material. Managed/external OAuth
binds stable subject/account/user identity, not rotating access/refresh tokens.
Existing executors cannot follow another account selected in the daemon's auth
cache. File-backed roles are frozen into the execution configuration; edits are
checked against the original sources before boot, admission and dispatch, and
on the 15-second heartbeat. Detected drift contains execution and retains
uncertain reservations; it does not authorize replay.

Enabled HTTP MCP connections require an explicit, present bearer or authorization
header source. Anonymous HTTP MCP and implicit MCP OAuth are unsupported in
durable scopes until their identities can be pinned.

This is a **control-plane boundary, not OS tenant isolation**. The fingerprint
does not freeze tool binaries, arbitrary files or remote server implementations,
and checks do not fence already-running external effects. Use separate OS
users/containers and restricted mounts, homes/credential stores, environment,
VFS and network policies for mutually untrusted tenants. A bearer still grants
powerful server-owned tools. No per-request tenant, policy, credential, cwd,
environment or sandbox overrides are added.

Automatic reconciliation of unknown execution outcomes, scoped per-turn
environment, complete provider-invoice coverage and clamp support remain
separate work.

## Durable observation replay

Scoped sessions persist bounded kernel observations before exposing them to
HTTP/SSE subscribers. Sequences survive restart; memory-only sessions retain
their existing behavior. The retained window is at most 256 events/1 MiB, with
individual observations bounded to 64 KiB and explicit omission/gap markers.
This is not unlimited transcript retention or execution replay.

After takeover/restart, an explicit omission observation warns that events may
have been lost between kernel activity and durable observation commit. A failed
append contains execution and retains uncertain capacity. Lease-fenced
compare-and-append prevents stale writers from extending the log. Saved
observations never authorize dispatch.

Raw kernel completion observations are **not** committed HTTP terminal outcomes:
clients must reconcile the turn record or terminal webhook. A later failed
terminal transaction can still leave the turn unknown. Replay responses identify
themselves as observation-only; they are not billing or containment evidence.

## Invocation-local accounting

New durable HTTP turns explicitly enable accounting at admission. Before each
native sampling/compaction call, the kernel must commit a fresh invocation
UUID under the current fenced session lease, dispatched turn UUID and bound
process ID. An identical begin is read-only and **never authorizes another
provider call**. Metadata records the provider ID, requested model/service tier
and sampling/compaction purpose; no prompt, URL, credentials or caller billing
override is stored. The process's sink is installed before retained input is
submitted. Plain streaming cannot bypass an installed sink.

Native delegated/review descendants receive a root-bound sink before their
startup loop can dispatch. Registration commits fresh process lineage under the
root's active metered turn and permanently binds the child to the scope/policy.
Nested descendants must name an already authorized parent. Invocation records
retain the actual child process and `source_turn_id`, while `request.turn_id`
identifies the owning root turn. Descendant histories cannot be resumed as
standalone HTTP sessions or transferred to another root turn.

A completed provider response's receipt commits before completion is forwarded
to the kernel. Its observed response ID, optional server model and normalized
input/cache-write/cache-read/output/reasoning/total counters are immutable.
Identical receipt retries do not add usage; changed receipts conflict.
Malformed negative/cache-inconsistent counters are rejected from the subtotal;
that rejection does not prove the provider did not charge.
Provider-adapter normalization may derive a total from reported input/output;
it never substitutes history estimates or cumulative deltas.

Completed responses without usage retain `usage: null`. Interrupted, failed or
unobserved calls remain `unknown`, never a zero-cost assertion. Execution loss,
quarantine, finish and operator resolution atomically fence outstanding starts
to unknown while preserving committed receipts. Failed accounting writes stop
further provider dispatch and retain uncertain execution capacity. Recovery
closes execution; it cannot manufacture, replace or erase usage.

Nonempty response IDs are deduplicated by scope/provider/response ID. A later
invocation retains its observed receipt with `duplicate_receipt_of` and is
excluded from the reported subtotal. This is an ambiguity marker, **not proof
that the duplicate was free**. Missing IDs cannot support cross-invocation
deduplication. Checked subtotal overflow returns `known_usage: null` and
`totals_overflowed: true`, never a wrapped/saturated amount.

`GET /api/sessions/<session>/turns/<turn>/accounting` returns the bounded records
and a summary, under ordinary session authentication (plus the operator header
in recovery-only mode). At most 128 logical invocations may be recorded per
turn; exceeding the limit stops before another provider-client call. Records
survive closure/restart and are not implicitly evicted. Legacy/pre-metering turns
return `enabled: false` and `summary: null`: missing history is not zero usage.

The summary separates unique reported invocations, completed-without-usage,
pending, unknown and duplicate receipts, along with a **partial reported
subtotal**. If no unique reported usage exists, `known_usage` is null, not a
synthetic zero. Cumulative `turn.usage`/context-window counters retain their
existing meaning and are not billing inputs. New terminal webhooks carry this
accounting summary in the same transaction as their outcome; full receipts remain on the
authenticated accounting endpoint. Existing stored webhook bytes are unchanged.

### Coverage is not a provider invoice

Every summary explicitly says `billing_complete: false`. New turns persist
`coverage: "root_and_descendant_logical_sampling_and_compaction"`; old turns keep
their original direct-only coverage tag. The boundary is a logical client
request, not an assertion that every provider-side charge is observable.
Metered clients disable automatic transport/auth/fallback retries; a new outer
sampling call needs a new durable begin. It excludes
planning/reflex work, root startup/autonomous calls,
catalogue traffic, tools, clamp, memory-only sessions and `/api/trigger`.
Cancellation can race a provider charge or an unobserved receipt. Even when all
recorded calls have usage, excluded work/retries may incur additional costs.

Requested model/service tier are not proof of the charged SKU/tier.
Operator-only versioned prices and immutable external receipt imports are
described in [Financial evidence](http-session-financial-evidence.md).
They provide exact integer estimates and independently attested charged amounts,
not a credit reservation, invoice verifier or native-receipt overwrite endpoint.
An adapter must still establish coverage and verify provider evidence.
Do not price a full provider bill from this subtotal or treat any missing,
unknown, duplicate or overflowed data as a refund/free request. Private receipt
metadata also needs authorization/redaction before frontend projection.

## Durable terminal webhooks

Terminal **Finish** and operator **Resolve** insert immutable outbox events in
the same transaction as their outcomes/audits. If the insert fails, the whole
transition rolls back and execution capacity stays reserved. Identical retries
produce no additional event. `unknown` is not terminal and produces no callback.
No historical events are backfilled from before migration 29.

Events are recorded even with delivery disabled. To deliver all pending events
in this scope, configure a trusted server-owned target:

```sh
# Supply a random, server-only key of at least 32 bytes via this variable.
chaos serve --sessions --session-scope project-worker \
  --session-webhook-url https://adapter.example/internal/chaos
```

The default signing variable is `CHAOS_SESSION_WEBHOOK_SECRET`; select another
with `--session-webhook-secret-env`. Do not put secrets in CLI arguments or
callback URL userinfo. The signing key must differ from bearer/recovery tokens.
HTTPS is required except literal loopback HTTP for local
development. The URL and signing identity are immutably bound to the scope by a
private digest; changing either fails startup, including after delivery. Only
the digest is stored, not the URL/key. Omit the URL to pause delivery without
forgetting events. Recovery-only mode may deliver/resolve with the original
target/key without restoring model credentials.

One bounded worker claims due events with independent 60-second fenced leases.
HTTP attempts time out after 10 seconds; only 2xx response headers authorize a
delivery acknowledgement. Redirects are never followed, response bodies are not
collected/logged, and model credentials/cookies are never attached. Non-2xx and
transport failures retry with storage-clock exponential backoff (2 seconds to
1 hour). Retries continue indefinitely; there is no implicit eviction, dead-letter
discard or forced re-drive endpoint. Storage/acknowledgement loss retains the
event. Graceful shutdown or a crash can leave a lease to expire before retry.
Delivery does not consume execution permits or authorize kernel submission.

### Wire contract

POST carries the exact stored JSON bytes and these headers:

- `X-Chaos-Event-ID`: stable event UUID, also present in the body.
- `X-Chaos-Timestamp`: signing time as Unix seconds; fresh on each attempt.
- `X-Chaos-Signature`: `v1=<lowercase HMAC-SHA256 hex>`.
- `X-Chaos-Delivery-Attempt`: storage-counted claim attempt (not a signed authority).

The MAC input is the byte concatenation
`timestamp + "." + event_id + "." + raw_body`. Verify the MAC in constant time,
check a bounded timestamp skew (for example 5 minutes), and require header/body
event IDs to match. Keep daemon/receiver clocks synchronized.

The version-1 body contains `event_id`, `type`, `occurred_at_ms`, `scope`,
`session_id`, `process_id`, `turn`, and `resolution_id`. `turn.terminal` carries
the immutable terminal turn (including its bounded outcome); recovery includes
its resolution UUID. A no-turn resolution emits `session.resolved` with a null
turn. Operator actor/reason evidence, lease tokens and credentials are omitted.
These remain **private runtime outcomes**, not public narration or
complete billing telemetry. The trusted adapter must authorize and redact
any frontend projection.

Delivery is **at least once, not exactly once or ordered**. A receiver may commit
and then lose its HTTP response or the daemon may lose its storage acknowledgement.
Persist/deduplicate `event_id` atomically with receiver effects before returning
2xx. Multiple attempts can overlap after lease expiry; duplicate payload bytes
are identical. Never resubmit a model turn because delivery is absent or duplicated.

`GET /api/sessions/deliveries` returns at most 256 delivery-status records with
attempt counts, deadlines and acknowledgement timestamps, without payloads or
lease tokens. `?after=<event UUID>` pages the UUID-sorted metadata snapshot; it
is **not an append-only replay cursor**. Refresh the listing to find new events.
Normal bearer authentication applies; recovery-only mode also requires the
operator header on this read. Scoped observation replay is separately durable
and bounded; callback delivery is not SSE cursor acknowledgement.

Callback networking is daemon control-plane I/O, not mediated by a model's tool
sandbox or execution semaphore. Restrict daemon egress/proxy access and use only
trusted operator-configured destinations. Startup rejects a signing variable
exposed by the base command environment or explicitly selected by MCP tools.
Keep it excluded from delegated roles, shell profiles, hooks and all other tool
environments as well. This check is not an OS secret-isolation boundary.

## Operator recovery

`GET /api/sessions/<session_id>/recovery` inspects the persisted session, its
quarantine `recovery_epoch`, any committed resolution, and whether the local
supervisor has stopped. Normal bearer authentication is required; lease tokens
are never returned. `can_resolve` is a hint, not containment evidence or authority.

Resolution is disabled unless a separate nonempty `--recovery-token` (or
`CHAOS_RECOVERY_TOKEN`) is configured with `--session-scope`. It must differ from
the ordinary bearer token. POST also requires both the ordinary bearer and
`X-Chaos-Recovery-Authorization: Bearer <operator token>`, plus the current
`X-Chaos-Daemon-ID`. Authorization precedes body reading.

After independently containing the executor **and external subprocesses/remote
tools**, POST `/api/sessions/<session_id>/recovery` with:

```json
{
  "resolution_id": "caller-generated canonical UUID",
  "expected_process_id": "process UUID from inspection, or null",
  "expected_turn_id": "active turn UUID from inspection, or null",
  "expected_recovery_epoch": 1,
  "containment_confirmed": true,
  "actor": "operator identity",
  "reason": "How executor and external effects were independently contained"
}
```

Storage records an operator-supplied actor (1..256 bytes), reason (1..4096 bytes)
and its own timestamp;
it **cannot verify the containment assertion or claimed actor identity**. Do not
include secrets in evidence or expose the operator credential to callers/tools.
This HTTP guard is not a replacement for tool environment/sandbox isolation.

A live supervisor is always rejected. Stop the daemon, contain its external work,
wait for lease expiry if needed, restart into quarantine, then inspect and resolve.
New quarantines and ownership takeovers increment the epoch, so stale assertions
conflict even when process/turn IDs are unchanged. Reinspect and reconfirm after
such changes; never mechanically refresh a stale assertion.

Success permanently closes the session and marks its active turn `interrupted`
without claiming successful completion. Bounded partial result/usage is retained
when possible; an oversized snapshot is explicitly marked omitted. Turn
tombstones and the resolution audit remain immutable. No-turn startup/autonomous
quarantine releases its orphan reservation without manufacturing a turn.

Daemon tasks own resolution writes and ingress reservations through HTTP
disconnects. Failed/ambiguous commits retain execution capacity. Read the audit
and retry only the identical decision with the same resolution UUID; a new
resolution UUID is not permission to overwrite a committed decision. The local
permit is released only after observing durable closure. Resolution never resumes
history or replays input; continuing work requires an explicit new session.

### Draining legacy or changed-policy scopes

Start with `--sessions --session-scope <old namespace> --session-recovery-only`
and both bearer/recovery credentials. This mode reads the stored policy instead
of adopting the current one, requires the operator header on **all protected
routes**, and rejects triggers and execution mutations. Audited recovery,
price/evidence registration and archive operations remain available with the
operator header.
It never boots or resumes a kernel; orderly released bound history is quarantined
for explicit containment/resolution. Execution-only login, clamp and git-trust
checks are skipped, so obsolete model credentials need not be restored.
The correct journal backend/home must still be selected, and live ownership or
capacity conflicts still fail startup.

Legacy history has no verified policy and remains non-executable. Closing it
does not erase its tombstones or let that namespace adopt a new policy.
Arbitrary CLI history cannot be imported into a bound scope; only previously
registered matching process history can resume.

## Backend validation

### Deployment and restart drills

This remains an experimental control plane, not a production containment or
billing guarantee. Run drills with disposable homes, scopes, provider credentials
and tools that cannot mutate production. Keep the same server-owned policy and
working directory across an orderly restart. Use one daemon owner per scope;
do not configure a restart loop to invent namespaces or overwrite policy.

Before deployment:

- Keep the runtime database and transcript history on durable storage. Update
  the CLI and journald together (protocol 12, migrations through 34); migrations
  are forward-only here, not a supported old-binary rollback.
- Isolate tools with OS users/containers, restricted mounts/environment and
  egress. Protect the bearer, operator and signing credentials independently;
  keep them unavailable to tools. Do not expose plaintext HTTP outside a
  trusted loopback/proxy boundary.
- Budget `--max-concurrent` for *all* outstanding unknown work at restart.
  `--max-sessions` includes closed tombstones; a larger limit does not remove
  them. There is no safe automatic retention/purge mechanism yet.
- Receiver deduplication must commit the event UUID with its effects before
  returning 2xx. Persist its deduplication history independently of daemon
  restarts. Monitor delivery attempts/deadlines via the authenticated listing;
  refresh all pages, not just the last UUID cursor.
- `/api/health` is listener health, not proof of storage availability,
  containment, accounting completeness or callback delivery.

Drill these boundaries before using an adapter:

1. **Orderly idle restart:** record the daemon/session/turn IDs and completed
   outcome; gracefully stop and wait for exit. Restart with the same policy and
   database. Confirm a new generation and unchanged outcome. Retrying that
   turn UUID must return saved state, not execute it. Only an explicit fresh
   turn may resume orderly released history.
2. **Active crash:** kill the daemon during a harmless long-running invocation.
   Independently stop/reap the executor's process group/container and inspect
   any remote tools; daemon death alone is insufficient. A restart before lease
   expiry may conflict. After expiry (up to 90 seconds after the last renewal),
   inspect in recovery-only mode: active work must be unknown, its reservation
   held, and missing receipts unknown rather than zero. Neither a retry nor a
   fresh turn may execute in that session.
3. **Explicit recovery:** inspect the current process/turn/epoch and independently
   reconfirm containment. Submit the audited operator decision described above.
   Closure should release capacity while retaining turn/receipt history. It
   must not resume the process. Continue only through a new session.
4. **Receiver outage/lost acknowledgement:** have a disposable receiver persist
   an event and deliberately withhold its response. Restart the daemon during
   delivery. After the delivery lease expires, the same event UUID and exact
   body must retry; receiver effects remain single through deduplication.
   This must not create a model invocation or release unknown execution capacity.

On incidents, stop ingress and independently contain external execution before
resolution. Do not edit lease timestamps, drop tombstones or resubmit unknown
turns. Omit the webhook URL to pause delivery; restoring delivery requires its
original URL/key, not a new destination bound to old events.

Use backend-native consistent backups, including SQLite WAL where applicable,
and preserve the home/history and credential identities needed for recovery.
**Restoring an older database can lose deduplication and recreate execution
authority**; a backup is not a replay-safe rollback. Restore into an isolated,
recovery-only environment without tools/normal ingress, reconcile external
effects and receiver history independently, and never silently return stale
history to service.

The automated crash drill SIGKILLs a real child test process holding a fake
executor, pending invocation and leased event, then reopens SQLite. It verifies
unknown/no-replay recovery, receipt preservation, independent delivery fencing
and audited capacity release. It does **not** prove real subprocess/container or
remote-tool containment; the deployment drills above still require that evidence.
The [real-daemon drill](http-session-e2e.md) additionally exercises the native
kernel and shell, steering, signed callbacks, actual subprocess interruption and
real lease-expiry recovery against an isolated mock provider.

The HTTP durability suite uses SQLite by default. Set `TEST_DATABASE_URL` to a
**disposable PostgreSQL database** to run the same scenarios through the
production PostgreSQL journal client: acceptance/terminal commit failures,
restart quarantine, orderly handoff, writer fencing, dispatch cancellation and
HTTP disconnects, plus operator authorization, resolution failures, stale
quarantines, audit-bound recovery, policy drift, legacy rejection and
operator-only draining, outbox rollback, delivery fencing and signed HTTP
retries after lost acknowledgements. Invocation barriers, immutable receipts,
pending-call takeover, duplicate provider response IDs and explicit
missing/overflowed usage are covered as well.
Each HTTP fixture creates an isolated schema, runs the runtime migrations and
drops that schema on success; the test role needs schema-creation
permission. A failed test may leave its schema for diagnosis.

```sh
TEST_DATABASE_URL="$PG_TEST_URL" cargo test -p chaos-httpd --lib sessions::tests::durability
TEST_DATABASE_URL="$PG_TEST_URL" cargo test -p chaos_journald --lib postgres
env -u TEST_DATABASE_URL cargo test -p chaos-httpd --lib sessions::tests::durability
```

The journald PostgreSQL suite also verifies that competing claims/admissions
have one winner and simultaneous dispatches authorize exactly one submission.
It also races policy bootstrap against legacy claims and verifies that closed
history cannot cross tenant/policy boundaries.
It writes to the test database's default schema, so never use a production URL.
Linux CI runs both PostgreSQL suites against a disposable PostgreSQL 18 container,
in addition to the default SQLite coverage.
