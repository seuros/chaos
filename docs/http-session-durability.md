# HTTP session durability

The experimental `chaos serve --sessions --session-scope <namespace>` API uses
journald for admission, dispatch identity, terminal outcomes and ownership.
Without a scope the existing memory-only behavior is unchanged. Keep the daemon
generation precondition: after restart, read saved state before retrying any turn.

## Storage contract

`JournalClient::execution` exposes the same typed control-plane operations over
the SQLite journald Unix socket and the direct PostgreSQL backend. Migration 26
adds session and turn tables to the existing runtime database. Journald protocol
7 prevents accidentally talking to a sidecar that predates HTTP integration.
Conversation transcripts remain in the existing append-only journal.

A server selects a stable configuration/tenant `scope` and a unique owner ID for
each daemon incarnation. Neither value is permission to switch provider,
credentials, billing, tools, cwd or sandbox. Applications must not pass lease
tokens through the public HTTP API.

1. **Claim** a caller-stable session UUID and obtain a fenced lease.
2. **BindProcess** persists an immutable process mapping before admission.
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

Scope selection is an operator contract: use a separate stable name for each
configuration/tenant and do not change credentials/provider/cwd/sandbox/tools
under a retained name. No per-request policy overrides are added.
Replay/SSE durability, automatic reconciliation of unknown outcomes, scoped
per-turn environment, billing telemetry and clamp support remain separate work.
