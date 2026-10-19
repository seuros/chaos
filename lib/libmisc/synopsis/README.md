# Synopsis and workflow core

The existing `Synopsis` / `Node` / `Runner` API executes single-use actions
through `mcp_host::bt::{Tree, Scope, Job}`. It preserves the flat agent
synopsis contract and is also used by recall.

## Declarative input

`tree::TreeNode` is the shared, strictly decoded input for tool and agent trees.
Every node has an ID. Supported types are `sequence`, `fallback`,
`parallel_all`, `race`, `timeout`, `retry`, `condition`, `call`, `ask`, `sample`,
and `job`.

```json
{
  "type": "sequence",
  "id": "inspect",
  "children": [
    {
      "type": "call",
      "id": "read",
      "tool": "read_file",
      "arguments": {},
      "bindings": [{"target": "/file_path", "source": "/input/path"}]
    },
    {
      "type": "condition",
      "id": "has_result",
      "pointer": "/read",
      "predicate": {"op": "exists"}
    }
  ]
}
```

`ValidatedTree::new` validates the whole definition without executing leaves:

- At most 128 nodes, depth 16, 16 concurrent leaves, and 256 leaf attempts.
- Retry counts include the first attempt and are limited to three per decorator.
  Nested retry costs multiply and count toward the total attempt budget.
- IDs and result keys are unique. Successful result-producing leaves store at
  `result_key`, defaulting to the node ID; `input` cannot be overwritten.
- Bindings use RFC 6901 pointers, not expressions or string interpolation.
  Missing sources are structured failures, not null/default substitutions.
  Sources always read the result board; overlapping targets are rejected.
- Tool trees cannot contain agent jobs. Agent trees must reference every
  declared job exactly once and cannot contain call/ask/sample leaves.
- Retried calls require affirmative trusted idempotency metadata. Retried
  jobs require explicit `allow_replay=true`. Ask/Sample cannot be replayed.
- Known runner and authority-changing control calls are refused, including
  namespaced forms. Adapters must also check resolved canonical names.

Preflight is **not authorization**. Tool visibility, mode, group, sandbox,
approval, provenance, concurrency and retry safety must be rechecked by the
current dispatch adapter on each attempt. Ask is additional input, never an
authorization grant. Upstream requester APIs own negotiated interaction
capabilities and form/result validation.

`tree::compile_workflow` performs this complete preflight and translates tool
definitions to upstream `Node<Step>` for a `TreeTool`. It uses fallible argument
resolution and stable step IDs; it creates no scheduler or public registrations.
The embedding host must supply private, policy-aware leaf adapters. Missing
bindings fail before dispatch and carry a serialized `binding_error` diagnostic.

## Live run state

Keep one `runs::RunRegistry` on each native/authenticated MCP session, never in
a global catalog. Workflow and agent runs share its four-active-run budget.
Status and cancellation only inspect the owning registry. A run ID is not a
bearer credential and there is no restore/replay API.

An executor holds a `RunLease` and observes its cancellation token. It records
started/awaiting-input/terminal attempts, then completes only after cleanup.
Dropping its future/lease reports interruption and cancels work. A cancellation
request remains `cancelling` until the executor actually terminates.

`RunLease::recorder()` gives SDK observers and native adapters a cloneable weak
attempt recorder. It cannot finish a run or retain the session/executor. Late
callbacks cannot rewrite a terminal run; abandoning the lease still interrupts
it even when recorder clones remain alive.

Session teardown calls `shutdown()` even if background work still holds
registry clones. Last-handle drop also cancels work. Existing session maintenance
calls `collect_expired()`; all status/start/cancel operations enforce expiry too.
Terminal snapshots expire after one hour and are capped at 128 per session.
Snapshots are at most 1 MiB including metadata, with explicit truncation.
Full outputs remain governed by existing output policy; this registry logs no
payloads.

The native `run_workflow` handler is wired under the opt-in `workflows` group.
`workflow_status` and `cancel_workflow` remain available for owned runs after
group disable. Private SDK adapters use ordinary native dispatch, re-reading
mode, permissions, visibility and replay metadata after waiting for a
session-shared execution permit. Trusted containers do not hold leaf permits;
model-supplied names or MCP metadata cannot enable this. Input and snapshots
are not duplicated into the parent tool log.

Native Ask uses the existing `request_user_input` dispatcher, including its
current mode/group policy and session-shared execution permit. Empty forms
become an explicit Continue/Cancel question; boolean and single-select enum
forms support up to three fields and three choices per field. Answers are
converted to their declared JSON types and validated by the SDK. Free text,
numeric, multi-select and larger forms fail explicitly before prompting;
fallback can handle unsupported interactions. Declining or dismissing is not
approval. Cancelling/dropping the request closes its pending input receiver
and releases the turn mailbox slot without deleting a live replacement.

Native Sample makes one isolated text-only request through the owning session's
provider, auth, egress, telemetry and invocation-accounting path. It rechecks
workflow visibility and recovery policy after acquiring the existing session
execution permit. No tools, native server tools, parent conversation history,
shared WebSocket continuation, agent or model/tool loop are available. Clamped
CLI transports fail explicitly instead of switching authentication or launching
a subprocess. Unsupported sampling extensions also fail; fallback can handle
these failures.

Native sampling accepts 1–16384 output tokens and at most 128 KiB of input plus
instructions. The cap is sent to the provider; returned content is additionally
limited to 256 KiB and 4096 events. Any tool/action response is refused, partial
streams without completion fail, and dropping the interaction cancels the
existing provider stream. Usage contributes to session totals without replacing
the main conversation's context/compaction baseline. An installed accounting
sink must admit the call and commit its completion receipt before success.
Provider adapters may reject a token cap they cannot support; it is never
silently removed.

The kernel exposes a narrow `Process::attach_workflows` bridge for an already
authorized external MCP session attached to an existing active native turn.
Its opaque `WorkflowScope` binds run/status/cancel to that MCP session; another
scope's run IDs convey no access. Native and external execution share the same
policy-aware leaf adapters and SDK tree executor. External Ask/Sample recheck
kernel policy after their execution permit, then use only the original request's
negotiated SDK requester (including related-task routing); neither a native
provider nor native picker is a fallback. `_meta` is context, never authority.

The existing-turn attachment refuses idle/ending turns. For a dedicated idle
workflow process, `Process::open_workflows` instead submits a host-only admission
through the normal kernel submission loop. The bounded queue is not a wire Op
and is never populated from `_meta`. Admission refuses busy processes and
journal/machine recovery, inherits current constrained settings, and registers
a real model-free operation/approval context. It cannot enable groups, broaden
permissions, choose a provider, steer an existing conversation, or invoke a model.
Background completions do not authorize automatic native-model continuation.
Abandoned admission, scope closure and native shutdown clean up that context.

Normal turn completion and interruption revoke the scope before removing turn
permissions; missing authority fails closed rather than rebuilding defaults.
Retained status/cancel remain scoped after group disable or turn completion.
The embedding server keeps one owning scope on its dedicated workflow process
and explicitly closes it on MCP session expiry/DELETE, not on temporary SSE
disconnect. Clones share the run budget and retained results across calls.

Actual SDK task fixtures cover related-task requester state (`input_required`),
owned results, cross-session refusal, worker-drop cancellation and reusable run
budgets. Older clients retain synchronous execution without negotiating tasks.
Public mcpd registration, per-call action-approval/event routing, transport
session teardown and the Apps dashboard remain integration work. The bridge is
not yet advertised as a complete external endpoint. No execution is restored
after restart.

## Agent trees

`run_synopsis` accepts `jobs` and exactly one of legacy `mode` or nested `tree`.
Both use the same bounded SDK execution path. Job results keep declaration
order; `run_id` identifies per-attempt records in the session-owned status API.
Each activation builds a fresh child configuration from current parent policy.

```json
{
  "jobs": [
    {"id": "inspect", "message": "Read the module and summarize it.", "allow_replay": true}
  ],
  "tree": {
    "type": "retry",
    "id": "retry_inspect",
    "attempts": 2,
    "child": {"type": "job", "id": "inspect_step", "job_id": "inspect"}
  }
}
```

Delegation still requires explicit user authorization. `allow_replay` authorizes
only the requested job retry, not additional tools or broader child permissions.
No child or action from a previous attempt is reused.
