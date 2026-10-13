# HTTP session durability: storage foundation

The experimental `chaos serve --sessions` API still retains its session/turn
records in memory. This change introduces the **journald ledger prerequisite**;
HTTP admission and recovery are not yet wired to it. Keep the existing daemon
generation precondition and do not retry a lost HTTP turn across daemon restarts.

## Storage contract

`JournalClient::execution` exposes the same typed control-plane operations over
the SQLite journald Unix socket and the direct PostgreSQL backend. Migration 26
adds session and turn tables to the existing runtime database. Journald protocol
6 prevents accidentally talking to a sidecar that predates these operations.
Conversation transcripts remain in the existing append-only journal.

A server selects a stable configuration/tenant `scope` and a unique owner ID for
each daemon incarnation. Neither value is permission to switch provider,
credentials, billing, tools, cwd or sandbox. Applications must not pass lease
tokens through the public HTTP API.

1. **Claim** a caller-stable session UUID and obtain a fenced lease.
2. **BindProcess** persists an immutable process mapping before admission.
   Live and unknown sessions cannot map to the same process.
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
by a previous owner. The HTTP supervisor must stop on ownership/storage loss,
retain its capacity reservation, and perform bounded containment. It must not
treat a rejected heartbeat as proof of cancellation.

## Bounds and follow-up

Each session keeps at most 256 turn tombstones. Outcomes are bounded to 32 KiB
of serialized JSON; list pages to 256 sessions. There is no implicit tombstone
eviction or deletion that could make an old turn UUID executable again.

The next integration step is to wire daemon-owned HTTP admission/dispatch and
terminal reconciliation to this contract, renew leases independently of HTTP
requests, restore persisted IDs/outcomes, and lazily resume proven-idle history.
Unfinished sessions must remain unknown until containment is established.
Replay/SSE durability, scoped per-turn environment, billing telemetry and clamp
support remain separate work.
