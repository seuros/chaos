# acp-host

Host an ACP agent over caller-owned Tokio streams, using the official
`agent-client-protocol` SDK for wire types, dispatch and request correlation.
The crate has no production dependency on Chaos.

Implement `AgentBackend` to open sessions and `AgentSession` to process prompts,
cancel native work and shut it down. Start an `AgentServer` with `serve(reader,
writer)`. See the `echo` example for a standalone stdio agent.

Only stable ACP v1 core sessions are exposed. Backends own authentication,
trusted MCP configuration, tool policy and real execution. The host never
retries a prompt, automatically approves an action, or equates a cancellation
request with confirmed termination.

Connections bound encoded messages, admitted requests, sessions, queued updates
and pending permission requests. Slow/closed peers fail closed rather than
silently losing updates. Permission responses are tied to their originating
turn and checked against the offered options. A backend must finish its prompt
only after native work has actually reached the reported terminal state.

Logs must not be written to the protocol writer. Embedders retain responsibility
for driving shutdown and containing any work whose termination is unconfirmed.
