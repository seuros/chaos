+++
title = "Sub-agent lifecycle events"
summary = "Consume payload-free direct-child status updates from the exec JSONL stream."
+++

# Sub-agent lifecycle events

`chaos exec --json` emits `agent.status_changed` events independently of
collaboration tool-call items. A completed `spawn_agent` tool call means that
spawning finished, not that the child's work finished. Consumers do not need to
ask the parent model to call `wait_agent` to observe child completion.

```json
{"type":"agent.status_changed","parent_process_id":"67e55044-10b1-426f-9247-bb680e5fe0c8","child_process_id":"9e107d9d-372b-4b8c-a2a4-1d9bb3fce0c1","agent_nickname":"Ada","agent_role":"default","model":"example-model","status":"running"}
```

## Payload

Each event identifies the direct parent and child processes. Optional
`agent_nickname`, `agent_role` and `model` describe the child when available;
consumers should key their state by process ID, not nickname. The model identifier
does not attest to the provider's routing.

`status` is one of:

- `pending_init`: initialized and awaiting its first turn;
- `running`: processing a turn;
- `interrupted`: the current turn was interrupted;
- `completed`: the current turn completed;
- `errored`: the current turn failed;
- `shutdown`: the child was closed;
- `not_found`: the child could not be found.

Completion is not permanent termination. Sending new input can make the same
child run again. Reopening a closed child alone does not mean new work has
started. Shutdown does not retrospectively classify a completed turn as failed.
Consumers needing the last work outcome should preserve that separately from
whether the child is still open.

The lifecycle payload contains no prompts, reasoning, final messages or raw
error text. Other JSONL events may contain private task content; do not treat
the entire stream as redacted.

## Delivery boundaries

Events describe direct children, not a flattened tree of all descendants. They
are status observations, not collaboration tool results. Repeated observations
of the same status are allowed; consumers should upsert by child process ID.

Delivery requires an open parent invocation. These events do not keep the CLI
alive after its normal exit, wait for children implicitly, or provide a durable
subscription/replay API. Reattaching a parent does not synthesize snapshots of
already-active children; subsequent child status publications can still arrive
while the parent is registered. End-of-stream is not evidence that a child completed:
consumers should display unresolved activity as unknown or disconnected, not
successful.

The first status is published only after session initialization succeeds.
Failed initialization, including a required MCP server failing to start, is
reported by the spawn tool without creating a pending child lifecycle entry.
