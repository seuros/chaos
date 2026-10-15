# Retained-session financial evidence

Financial evidence is operator control-plane data, not execution authority.
All routes below require the ordinary bearer **and**
`X-Chaos-Recovery-Authorization: Bearer <operator token>`, including reads.
Mutations additionally require the current `X-Chaos-Daemon-ID`.
The daemon supplies the scope; bodies cannot override it. These operations
also work while draining in recovery-only mode and never boot a kernel.

## Versioned prices

`PUT /api/sessions/prices/<version>` registers an immutable schedule.
An identical retry succeeds; changing that version conflicts. GET at the same
path reads it. To change rates, register a new version, never edit old evidence.

The JSON body has:

- `version`, `provider_id`, `model`, `service_tier`: explicit bounded strings.
- `currency`: three uppercase ASCII letters. No currency conversion is implied.
- `tokens_per_unit`: integer in 1..1,000,000,000.
- `prices`: nonnegative integer **nanocurrency units per token unit**, with keys
  `uncached_input`, `cache_creation_input`, `cached_input`, `ordinary_output`,
  and `reasoning_output`.
- `source_sha256`: lowercase SHA-256 of the operator's rate-source document.
- `actor`, `reason`: operator-supplied audit attribution, not independently
  verified identity.

One currency unit is 1,000,000,000 nanounits. Estimates use checked integer
arithmetic, partition input/cache and output/reasoning into **disjoint** buckets,
sum all bucket numerators, then round upward once to a whole nanounit.
Aggregate input/output counters are not priced again on top of their cache or
reasoning subsets. The reported total token counter is not another charge.
No built-in market rates, automatic discounts, FX, tax or billing-period rounding
are applied. An explicitly configured zero rate is not inferred free usage.

## External provider reconciliation

POST `/api/sessions/<session>/turns/<turn>/billing` adds an immutable operator
observation; GET reads the bounded records. A request contains:

```json
{
  "reconciliation_id": "<canonical UUID>",
  "invocation_id": "<recorded invocation UUID>",
  "provider_receipt_id": "<external attempt/receipt ID>",
  "evidence_sha256": "<lowercase SHA-256 of provider evidence>",
  "model": "<observed charged model>",
  "service_tier": "<observed charged tier>",
  "usage": null,
  "charged": {"currency": "USD", "nanounits": "123000000"},
  "price_version": null,
  "actor": "<operator identity>",
  "reason": "<how provider evidence was verified>"
}
```

The amount above illustrates encoding, not a provider tariff. `nanounits` is a
canonical nonnegative decimal integer **string**, avoiding binary float and
JavaScript integer precision loss. At least observed usage or an observed
charged amount is required. Credits/negative amounts and corrections are not
silently interpreted as refunds; handle those in the external billing system.

`usage`, when supplied, uses the native six normalized counters: input,
cache-creation input, cached input, output, reasoning output and total tokens.
Missing usage remains null. To request an estimate, supply usage plus
`price_version`; the schedule must match the recorded provider and attested
model/tier, as well as charged currency if supplied. Requested model/tier are
not proof of charged model/tier.

The provider receipt ID is unique within scope/provider, even across sessions
and turns. Concurrent duplicate imports cannot double count. Identical
reconciliation-ID retries read the original record; changed bodies conflict.
Different actual wire receipts may be attached to the same logical invocation,
for example independently verified retry charges. At most 512 evidence records
may be attached to a turn.

External evidence never replaces a native receipt, changes an unknown native
call to completed, releases execution capacity, amends an existing terminal
webhook or authorizes a provider retry. Estimates and observed charged amounts
remain distinct. Reports deliberately retain `billing_complete: false`: imported
receipts do not prove that all startup, autonomous, remote tool or provider-side
charges have been discovered. Do not turn an empty report into a zero-cost claim
or automatically debit a customer from these unverified operator assertions.

## Archival

GET `/api/sessions/<session>/archive` reads the latest saved manifest/attestation.
POST with `{"operation":"export"}` exports eligible closed-session evidence.
The snapshot is writer-locked and includes outcomes, resolution audit,
invocations, receipt identities, delivery history, retained replay and financial
evidence with referenced price schedules. Undelivered notifications or unresolved
invocations block export; closure alone is insufficient.

Copy the returned **exact manifest JSON bytes** into independently durable,
access-controlled storage and verify its SHA-256. Then POST:

```json
{
  "operation": "seal",
  "manifest_digest": "<returned SHA-256>",
  "archive_reference": "<nonsecret external archive locator>",
  "actor": "<operator identity>"
}
```

The daemon does not fetch the locator or verify that the external copy exists.
Sealing is an immutable operator attestation of a matching current export,
not proof of backup integrity. Evidence changes between export and seal reject
the stale digest. Sealed sessions reject new financial evidence; already
committed identical reconciliation retries remain readable.

This is **non-destructive archival**: `payloads_compacted: false`. It deliberately
keeps original outcomes, receipts, tombstones and permanent process/scope
bindings so exact idempotency comparisons remain valid. It does not reclaim the
session cap, prune conversations or delete database rows. Automatic purge or
payload compaction without preserving these comparisons is not implemented.
Never drop history to make an uncertain turn executable again.
