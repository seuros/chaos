+++
title = "chaos-attested-review(7)"
summary = "Protected independent review orchestration."
+++

# chaos-attested-review(7)

## NAME

chaos-attested-review - host-managed independent review with protected model provenance

## DESCRIPTION

Use attested review when an MCP review service requires independently verified
account and model-family provenance. Do not fabricate provenance or submit it
through ordinary MCP calls.

The review gates appear only when:

- collaboration tools are enabled;
- the current mode permits mutation; and
- an MCP server exposes the `submit_review_verdict` capability.

If the review service uses leases or role claims, acquire one before starting a
run and pass that server's name and idempotency key.

## LIFECYCLE

Choose an exact configured provider and cached model with a known family.
Unknown-family models cannot pass attested review. The packaged Charm endpoint
recognizes `charm / deepseek-v4-pro` as `deepseek`; aliases and other endpoints
do not inherit that identity.

Set `model_family_overrides` per provider for exact model IDs, not gateway-wide
labels or inferred aliases.

For example:

```bash
chaos config set model_providers.example '{
  "name": "Example Provider",
  "base_url": "https://provider.example/v1",
  "model_family": "family-default",
  "model_family_overrides": {
    "model-alpha": "family-alpha",
    "model-beta": "family-beta"
  }
}'
```

Stored API keys work for providers supporting API-key authentication; no
subscription login or environment-variable copy is required.
Family resolution is: known catalog metadata, built-in registry, exact provider
`model_family_overrides`, then provider-wide `model_family` (unknown by default).
After changing provider metadata, refresh its catalog and start a new session.

Keep the returned `run_id`. After timeout or lost acknowledgement, use
`resume_attested_review`; do not start a replacement reviewer. Replaying the exact
original start request recovers the same run, not a new attempt.

Tool success is not verdict acceptance: inspect `acknowledged` and attempt states.
`state_unknown: true` is unresolved, not evidence that nothing was submitted.
Terminal failures do not respawn on replay. Only the creating ChaOS process can
resume or cancel the run; legacy runs without ownership are not resumable.

## REVIEW OUTPUT

The reviewer must return the requested review JSON: `patch is correct` approves,
`patch is incorrect` requests changes. Other correctness values or an empty
explanation prevent submission.

The provider must support strict JSON-schema output. Prose, fenced JSON, and
malformed output fail without submitting a verdict.

## MULTI-MODEL REVIEW

Review services may make leases session-local or restrict one role per caller.
In that design, a quorum uses one supervisor per role. Each supervisor claims
its role and starts one attested reviewer with the intended provider/account
and model. The service independently enforces its quorum and diversity policy.

## SECURITY

Reviewer processes:

- run in read-only Plan mode;
- cannot delegate to more agents;
- cannot contact their supervisor or use MCP or web tools;
- close after saving their output.

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-synopsis.7](./chaos-synopsis.7.md)
