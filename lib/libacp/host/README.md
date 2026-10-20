# acp-host

Host an ACP agent over caller-owned Tokio streams, using the official
`agent-client-protocol` SDK for wire types, dispatch and request correlation.
The crate has no production dependency on Chaos.

Implement `AgentBackend` to open sessions and `AgentSession` to process prompts,
cancel native work and shut it down. Start an `AgentServer` with `serve(reader,
writer)`. See the `echo` example for a standalone stdio agent.

Only stable ACP v1 APIs are exposed. Backends own authentication,
trusted MCP configuration, tool policy and real execution. The host never
retries a prompt, automatically approves an action, or equates a cancellation
request with confirmed termination.

Connections bound encoded messages, admitted requests, sessions, queued updates
and pending client requests. Slow/closed peers fail closed rather than
silently losing updates. Permission responses are tied to their originating
turn and checked against the offered options. A backend must finish its prompt
only after native work has actually reached the reported terminal state.

Logs must not be written to the protocol writer. Embedders retain responsibility
for driving shutdown and containing any work whose termination is unconfirmed.

Optional backend hooks expose caller-filtered model/mode selectors and durable
session IDs, list/load/resume. Defaults remain unsupported. Load replays public
updates with `update_wait` backpressure, without running previous work. Control
changes and replay share the prompt's exclusive session-operation boundary.
Backend `SessionInfo::available_commands` publishes the ACP slash-command
catalog on new/load/resume. Invocation remains a normal text prompt handled
by the backend; the host never interprets commands itself.

Connection, session and turn lifecycles use explicit `state-machines` transitions.
Transitions run synchronously under short locks, with I/O outside those locks.
Cancellation delivery and prompt completion are separate: a session stays busy
until its cancellation callback joins. Dropped live prompts and uncertain writes
invalidate the session instead of allowing another turn.

`ClientHandle` offers typed, capability-checked editor text reads and writes,
scoped to the current session/turn. Text is limited to 256KiB (and the connection
frame budget); replies have a finite deadline. A sent write without a successful
acknowledgement invalidates the session even if its backend swallows the error.
There is no retry, separate save request, revision token or atomic compare-and-swap.
ACP delegates writes to the client's filesystem API; buffer-only/no-save
behavior is not guaranteed and persistence remains client-controlled.
Backends must still validate native paths, permissions and approval before use.
Read errors retain their ACP error codes, so a backend can distinguish an
explicit resource-not-found response from other failures when creating a
file. Creation uses the same write request and uncertain-write containment;
the protocol does not provide exclusive-create or compare-and-swap.
