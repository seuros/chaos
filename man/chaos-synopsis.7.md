# chaos-synopsis(7)

## NAME

chaos-synopsis - FreeChaOS gate for coordinated sub-agent work

## DESCRIPTION

Synopsis is a FreeChaOS orchestration capability. It lets the active agent hand
several bounded jobs to sub-agents and treat their execution as one coordinated
operation.

Synopsis is not a command, configuration interface, or workflow language for
the user. The model-visible tool is a gate through which FreeChaOS requests the
capability. The kernel decides whether the gate is present and enforces the
session's collaboration, depth, agent-count, mode, sandbox, and approval
policies.

The user controls intent and authority through the conversation. The user can
ask FreeChaOS to delegate, use sub-agents, work in parallel, or avoid
delegation. FreeChaOS decides whether synopsis is appropriate and chooses the
jobs and coordination strategy.

## PURPOSE

FreeChaOS can use synopsis when a task benefits from coordinated sub-agent work
instead of a series of unrelated agent calls.

Typical uses include:

- completing dependent stages in order;
- checking independent concerns concurrently;
- trying another agent after one fails to start or complete;
- running competing approaches and abandoning work that is no longer needed;
- collecting sub-agent outcomes into one parent decision.

Synopsis is most useful when the work can be divided before execution begins.
FreeChaOS should keep work in the parent when the next action is an immediate
local step, when delegation would add no value, or when the workflow depends on
interactive user decisions between stages.

## COORDINATION

FreeChaOS can coordinate synopsis work in four general shapes:

| Shape | FreeChaOS behavior |
|-------|--------------------|
| Sequential | Complete dependent jobs in order and stop after a failure. |
| Parallel | Run independent jobs together and require all necessary work to complete. |
| Fallback | Try alternatives in order until one completes successfully. |
| Race | Run competing jobs together and stop work that is no longer needed after the race is decided. |

These shapes describe what FreeChaOS can do. They are not user-selectable
syntax. A user's request can express a preference such as "review this in
parallel," but the parent agent remains responsible for choosing a safe and
useful execution plan.

## THE GATE

The synopsis tool is a model gate, not an end-user tool.

The gate:

- is exposed only when collaboration is available to the active session and
  mode;
- does not authorize delegation by itself;
- cannot grant permissions or capabilities unavailable to the parent;
- cannot bypass sandbox, approval, role, depth, or agent-count restrictions;
- blocks the parent agent until the coordinated operation reaches an outcome;
- returns the child outcomes to the parent for synthesis;
- closes the agents created for the operation.

Explicit user authorization for sub-agents, delegation, or parallel agent work
is required before FreeChaOS crosses this gate. Complexity, a request for
thoroughness, or a request for research does not by itself authorize
delegation.

## USER EXPERIENCE

The user does not provide job identifiers, select internal control-flow values,
set gate parameters, or manage the child lifetimes created by synopsis.

From the user's perspective:

1. The user asks FreeChaOS to perform work and may authorize or forbid
   delegation.
2. FreeChaOS decides whether coordinated sub-agents materially help.
3. The kernel gates the operation through the active session policy.
4. The parent receives the child outcomes and reports a consolidated result.

Synopsis children do not each inject a separate completion message into the
parent conversation. Their outcomes are collected for the parent agent, which
is responsible for explaining the useful result, failures, and uncertainty to
the user.

## LIFECYCLE

Synopsis work is bounded to one coordinated operation. FreeChaOS waits for its
outcome and cleans up the agents created for it.

When one branch makes other work unnecessary, the unnecessary work is
cancelled. The same cleanup applies when the operation fails, is interrupted,
or exceeds its execution budget.

Synopsis does not create a persistent user-managed team. When persistent
agents, interactive steering, or individual lifecycle control are needed,
FreeChaOS uses the ordinary collaboration gates instead.

## POLICY

Synopsis inherits the active session's effective policy. A child cannot gain a
mode, tool, filesystem scope, network path, approval posture, or other authority
that the parent does not have.

Modes can remove the synopsis gate by narrowing the model-visible tool surface.
They cannot use synopsis to create delegation authority that was not already
available.

## OBSERVABILITY

FreeChaOS may report that it delegated work or ran checks concurrently when
that information helps the user understand progress or results. Internal job
names, scheduling choices, time budgets, and gate payloads are not a stable
user interface.

If synopsis cannot run because a gate or policy denies it, FreeChaOS should
continue locally when reasonable or explain why the requested delegation could
not be performed.

## SEE ALSO

- [chaos-modes.7](./chaos-modes.7.md)
- [chaos-support.7](./chaos-support.7.md)
