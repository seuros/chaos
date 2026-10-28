+++
title = "chaos-synopsis(7)"
summary = "Sub-agent orchestration gate."
+++

# chaos-synopsis(7)

## NAME

chaos-synopsis - FreeChaOS gate for coordinated sub-agent work

## DESCRIPTION

Use synopsis for a bounded batch whose jobs and coordination can be decided
before starting. Use ordinary agent tools when children need interactive
steering or continued work across turns.

## AUTHORIZATION

Explicit user authorization for sub-agents, delegation, or parallel agent work
is required. Complexity, thoroughness, and research requests alone do not count.
Children cannot exceed the parent's permissions or bypass approval, mode,
depth, or agent-count limits.

## CHOOSING WORK

Keep the immediate blocking step local. Delegate independent work that materially
advances the task, with concrete outputs and disjoint file ownership for edits.
Do not duplicate delegated work.

Use sequential jobs for dependent stages, parallel jobs for independent work,
fallback for alternatives, and race only when losing branches can safely stop.
Avoid synopsis if user decisions are needed between stages.

## COMPLETION

The call blocks until the operation returns and closes its children. Cancellation
also stops unnecessary branches; it does not undo their side effects.
Summarize useful results, failures, and uncertainty rather than exposing job
bookkeeping. If delegation is unavailable, continue locally when reasonable or
explain the limitation.

## SEE ALSO

- [chaos-modes.7](./chaos-modes.7.md)
- [chaos-support.7](./chaos-support.7.md)
