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
Journald protocol 9 prevents accidentally talking to a sidecar that predates
these isolation controls.
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

Replay/SSE durability, automatic reconciliation of unknown outcomes, scoped
per-turn environment, billing telemetry and clamp support remain separate work.

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
routes**, and rejects triggers and every session mutation except recovery.
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

The HTTP durability suite uses SQLite by default. Set `TEST_DATABASE_URL` to a
**disposable PostgreSQL database** to run the same scenarios through the
production PostgreSQL journal client: acceptance/terminal commit failures,
restart quarantine, orderly handoff, writer fencing, dispatch cancellation and
HTTP disconnects, plus operator authorization, resolution failures, stale
quarantines, audit-bound recovery, policy drift, legacy rejection and
operator-only draining. Each HTTP fixture creates an isolated schema, runs the runtime
migrations and drops that schema on success; the test role needs schema-creation
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
