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
and starts a read-only reviewer process.

`resume_attested_review` advances the persisted state machine. It can be called
again after a timeout or lost acknowledgement. A submission retry uses the
same stored JSON, idempotency key, and protected provenance.

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
