# MCP workflows and agent synopses

The MCP server uses the published `mcp-host` 0.13.1 SDK. `chaos mcp serve`
continues to serve stdio. `chaos serve --mcp` additionally mounts Streamable HTTP
at `/mcp` on the existing listener; it is off by default.

HTTP requires the existing operator bearer credential (`CHAOS_BEARER_TOKEN`).
The recovery credential cannot access MCP. Keep the default loopback binding;
non-loopback deployments need TLS at a trusted ingress. Browser hosts must use
an explicitly allowed origin (`--mcp-allowed-origin`, repeatable). Never put
credentials in a workflow, UI resource, or tool argument.

## Explicit discovery, unchanged authority

Workflow capability groups begin disabled. Call the top-level `enable_tools`
with `{"groups":["workflows","filesystem"]}` before submitting a tree that reads
files, or enable `agents` before `run_synopsis`. `disable_tools` changes this
MCP session only, and is still available when all run slots are busy.

Enabling a group does **not** grant sandbox, mode, approval or provider
permissions. Every call is dispatched through the kernel and rechecks current
policy after acquiring its normal execution permit. Action-specific approvals
are independent of an `ask` leaf. Controls, recursive workflows and nested
runner launches are prohibited inside trees.

Each external root request has a fresh, model-free native operation process.
No conversational prompt or automatic model turn is created. Group activation,
run status and the four-active-run budget belong to its actual SDK session;
event queues, permission contexts and approval requesters are not shared across
root requests. The normal read/mutation scheduler is shared by sibling roots,
so using separate processes cannot introduce concurrent mutations. Native and
external interactions follow their respective real
requesters, including negotiated task routing.

External resource listings are session-scoped too: cron entries must belong to
an owned process. Unattributed global spool rows are not exposed until a
trustworthy ownership index exists. Database IDs alone do not grant access.

## Tool trees

Call `run_workflow` with `tree`, optional object `input`, and optional
`timeout_ms` (default 120 seconds, maximum one hour):

```json
{
  "input": {"ready": true},
  "tree": {
    "type": "condition",
    "id": "ready",
    "pointer": "/input/ready",
    "predicate": {"op": "equals", "value": true}
  }
}
```

Composites are `sequence`, `fallback`, `parallel_all`, `race`, `timeout` and
`retry`. Leaves are `call`, `ask`, `sample` and `condition`. Calls accept literal
arguments and explicit JSON Pointer bindings; there is no expression evaluator
or string interpolation. Ask/Sample use only negotiated external-host support
on the MCP surface, never an invented capability or silent native fallback.

Call leaves use the canonical discovered tool name, such as `read_file` or
`mcp__skipper_upgrade__git_status`, not an API wrapper namespace such as
`functions.read_file`. For example, after enabling `filesystem`:

```json
{
  "tree": {
    "type": "call",
    "id": "read_operations",
    "tool": "read_file",
    "arguments": {
      "file_path": "/absolute/path/to/docs/mcp-workflows.md",
      "limit": 20
    },
    "result_key": "operations"
  }
}
```

Native Sample leaves use the current authenticated provider without tools or
conversation history; bound output with `max_tokens`. External Sample leaves
instead require the external host's negotiated sampling support.

Trees are bounded to 128 nodes, depth 16, 16 concurrent leaves and 256 attempts.
Retry is at most three total attempts and requires affirmative idempotency for
every descendant call. Unsupported interactions are ordinary failures that a
fallback can handle. Outputs obey existing tool policies and bounded snapshots.

## Agent trees

Use `run_synopsis` only with explicit user authorization to delegate. Supply
`jobs` and exactly one of a legacy `mode` or a declarative `tree`. Legacy modes
remain `sequence`, `parallel_all`, `fallback` and `race`. Agent trees use `job`
and `condition` leaves, not tool/interaction leaves. Every declared job must
appear exactly once. Retried jobs require `allow_replay: true` and receive a
fresh child identity per attempt. Existing child depth, mode, accounting and
cleanup policies remain in force. The default timeout is 30 minutes, at most
one hour.

## Ownership and cancellation

Results include a live-only `run_id`. `workflow_status` and `cancel_workflow`
accept `{"run_id":"…"}` and cover both workflows and synopses. Status and
cancellation remain available after a group is disabled. IDs convey no
cross-session authority, and neither history nor a session snapshot restores
execution after restart.

There are at most four active runs and 128 retained terminal snapshots per
session. Terminal retention is one hour; each snapshot is capped at 1 MiB with
explicit truncation. Cancellation is not completion until child/interaction
cleanup ends. Dropping a call/task cancels its operation; deleting or expiring
an SDK session closes all its work. An SSE reconnect alone is not session
termination. HTTP session limits, bounded queues, replay and principal pinning
are provided by the SDK, not a second scheduler.

The two root tools support optional negotiated MCP tasks. Older clients can
continue synchronous calls. Use standard `tasks/get`, `tasks/result` and
`tasks/cancel` only when the task extension was negotiated.

## External-host dashboard

Apps-capable hosts can load `ui://chaos/workflows`, linked from workflow,
synopsis and status tools. The static inline dashboard uses the Apps
initialize/initialized/teardown bridge, renders data through `textContent`,
requests no browser permissions and denies external network access.

The app-only `workflow_dashboard` returns at most ten caller-owned metadata
summaries. Payload details are intentionally omitted; paginate with its
`next_offset` or inspect a run through `workflow_status`. Cancellation uses the
same authorized endpoint as textual clients. Pending input must be answered
through the host's existing interaction flow: there are no approval-authority
buttons in the dashboard.

Without negotiated Apps, the app-only control is hidden/refused and structured
results retain a text fallback. ChaOS does not advertise an arbitrary Apps
renderer in its guest.

### External-host visual acceptance

Use a host that actually renders MCP Apps; record its name/version, negotiated
capabilities and transport. Automated bridge tests alone are not this check.

1. Enable `workflows`, submit the condition-only example above, and open its
   linked dashboard. Check that the run, step, attempt and terminal outcome render.
2. Refresh and verify that only this SDK session's runs appear.
3. If the host negotiates tasks and interactions, keep an owned run pending,
   cancel it from the dashboard, and verify cancellation finishes after cleanup.
   Answer approval/input requests through the host's normal interaction flow,
   never through a dashboard control that grants authority.
4. Repeat over stdio and authenticated `/mcp` HTTP. A host without Apps must
   retain useful text/structured results, not claim a successful visual test.
