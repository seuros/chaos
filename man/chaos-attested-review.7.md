# chaos-attested-review(7)

## NAME

chaos-attested-review - host-managed independent review with protected model provenance

## DESCRIPTION

ChaOS can run an independent reviewer and submit the result to an MCP review
service with host-attested provenance. The model never receives or constructs
the opaque account, model-family, run, or attempt subjects.

The review gates appear only when:

- collaboration tools are enabled;
- the current mode permits mutation; and
- an MCP server exposes the `submit_review_verdict` capability.

If the review service uses leases or role claims, acquire one before starting a
run and pass that server's name and idempotency key.

## LIFECYCLE

`start_attested_review` resolves an exact configured provider and cached model,
derives their opaque subjects inside the host, persists the run and attempt,
and starts a read-only reviewer process. A built-in identity registry, versioned
with the harness, fills missing catalog families for explicitly registered exact
provider/model IDs. Revision 1 registers `charm / deepseek-v4-pro` as `deepseek`
at the packaged Charm endpoint. No user configuration is needed for that entry.
Other endpoints, aliases, namespaces, and model suffixes do not inherit it.
Unknown models remain available for ordinary use but cannot pass attested
review's family check.

Provider-scoped `model_family_overrides`
mappings are operator-supplied TOML, exact-match only, are not a gateway-wide
label, and do not infer or rename models automatically.

For example:

```toml
[model_providers.example]
name = "Example Provider"
base_url = "https://provider.example/v1"
model_family = "family-default"

[model_providers.example.model_family_overrides]
"model-alpha" = "family-alpha"
"model-beta" = "family-beta"
```

Stored API keys count as configured credentials when the provider supports
API-key authentication; subscription login is not required. Stored credentials
must match a supported authentication method. The host still requires an exact
cached model and independently verifies account and model-family provenance.
Family resolution is: known catalog metadata, built-in registry, exact provider
`model_family_overrides`, then provider-wide `model_family` (unknown by default).
Registry and configuration values are applied when reading the raw catalog,
never persisted into it, so a harness registry update cannot leave stale derived
identities in the cache.

Changing provider metadata requires a refreshed catalog and a new session so
the host can re-read the exact configured model and family data.

`resume_attested_review` advances the persisted state machine. It can be called
again after a timeout or lost acknowledgement. A submission retry uses the
same stored JSON, idempotency key, and protected provenance.

After run creation, execution failures return public progress with the `run_id`,
attempt states, and `driver_error`, rather than discarding the recovery handle.
Tool-call success does not mean verdict acceptance: inspect `acknowledged` and
the attempt states. If progress storage cannot be read, `state_unknown: true`
means the outcome is unresolved, not that no submission occurred.
An exact replay of the original start request recovers the same persisted run;
do not change its key or payload to recover a lost response. Terminal failures
are not respawned by replay. Ownership checks still apply.

If ChaOS restarts during spawn or model execution, it recovers the uniquely
identified internal reviewer process or resumes its persisted rollout instead
of creating a duplicate reviewer.

`cancel_attested_review` stops a nonterminal reviewer and records the
cancellation reason.

Runs are fenced to the ChaOS process that created them. Another process cannot
resume or cancel a run even if it learns the run identifier. Runs created
before owner fencing are not resumable.

## REVIEW OUTPUT

The reviewer must return ChaOS's strict `ReviewOutputEvent` JSON. ChaOS maps:

- `patch is correct` to `approve`;
- `patch is incorrect` to `changes_requested`;
- the explanation to the verdict summary; and
- findings and confidence to a versioned findings object.

Any other correctness value or an empty explanation fails closed without
submitting a verdict.

## MULTI-MODEL REVIEW

Review services may make leases session-local or restrict one role per caller.
In that design, a quorum uses one supervisor per role. Each supervisor claims
its role and starts one attested reviewer with the intended provider/account
and model. The service independently enforces its quorum and diversity policy.

## SECURITY

Reviewer processes:

- run in read-only Plan mode;
- cannot delegate to more agents;
- cannot contact their supervisor or use MCP and web tools;
- are closed after their structured output is persisted; and
- submit only through ChaOS's trusted MCP metadata path.

Ordinary MCP calls cannot supply the reserved review-provenance metadata.

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-synopsis.7](./chaos-synopsis.7.md)
