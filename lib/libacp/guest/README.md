# acp-guest

Bounded Tokio connections to ACP agents, using the official
`agent-client-protocol` SDK for protocol types, dispatch and correlation.

The SDK's `process`, `stdio`, `schemars`, HTTP and RMCP integrations are not
enabled. Tokio owns I/O; the embedding application owns process execution.

## Boundaries

- Negotiates stable ACP v1, without automatically authenticating.
- Denies permission requests by default; an explicit application-owned
  `PermissionHandler` can decide individual requests. No filesystem or terminal
  access is advertised.
- Bounds incoming/outgoing JSON lines and unread session updates.
- Closes on update overflow, request timeout or dropped request future.
- Never retries a prompt, authenticates through an API-key fallback, or treats
  cancellation/disconnection as evidence that remote tools have stopped.

Drain session updates concurrently with requests, including session loading:
agents may replay history before acknowledging a load.

```rust,no_run
use acp_guest::{AcpConnection, ConnectionOptions};
use acp_guest::protocol::schema::v1::NewSessionRequest;

# async fn example(
#   reader: impl tokio::io::AsyncRead + Unpin + Send + 'static,
#   writer: impl tokio::io::AsyncWrite + Unpin + Send + 'static,
# ) -> Result<(), Box<dyn std::error::Error>> {
let (connection, mut updates) =
    AcpConnection::connect(reader, writer, ConnectionOptions::default()).await?;
let client = connection.client();
let observer = tokio::spawn(async move {
    while let Some(update) = updates.recv().await {
        // Private runtime observations: project/redact before showing users.
        let _ = update;
    }
});
let session = client
    .request(NewSessionRequest::new(std::env::current_dir()?))
    .await?;
connection.disconnect().await?;
observer.await?;
# Ok(())
# }
```

The transport owner must kill and reap a failed agent and independently account
for any external work. A fresh connection is not permission to replay uncertain
input. Loading saved history is capability-dependent and is never automatic.

## Antigravity acceptance goal

Antigravity's official ACP runtime is **not** the `agy` print CLI: it ships
`agy_acp_server.par` and the adjacent `localharness_external` executable.
Keep these together. Authentication belongs to that runtime and a dedicated
profile, not this crate. Do not read/copy OAuth tokens into Chaos or silently
fall back to metered Gemini/Google credentials.

The migration is accepted only after a live Chaos MCP tool round-trip, session
continuation, cancellation and uncertain-disconnect behavior are demonstrated.
A successful ACP initialization is not a completed migration. The existing
Antigravity print-mode backend remains selected until those checks pass.

**Current security gate:** live text/continuation/process reload passed, but
native file reads bypassed deny-all permission callbacks. Do not treat this
crate as native-tool containment or enable the Chaos backend on that basis.
Separate live tests verified a global `PreToolUse` hook denying `view_file`,
including an exit-1 hook-failure case. This remains a limited experiment:
Production MCP policy and full action/containment coverage are not established.
Additional October 17 live probes verified hook denials for native
`run_command`, `create_file` and `read_url_content`, with unchanged mutation
fixtures and zero requests to a private URL fixture. These unconfined checks
do not establish OS containment or policy-file protection.
An opt-in controlled MCP fixture and an isolated real-kernel bridge test now
demonstrate exact-target MCP reads, continuation and native-read denial.
These do not yet constitute a production model adapter or sandbox/egress parity.
A separate strict-proxy probe also verified live canonical instruction
replacement after configuring httplib2 to trust the ephemeral Chaos CA.
That unconfined compatibility check does not establish OS containment.
Native macOS proxy-only and protected-profile OS fixtures now pass, after
fixing platform argument construction and a policy-parent rename bypass.
However, the official ACP runtime fails local harness binding under that
strict policy; the matching unconfined control succeeds. Supported bounded
runtime IPC is still unverified, so production ACP/HTTP execution stays disabled.
See [`antigravity-acp-status.md`](../../../docs/antigravity-acp-status.md) for
the failed acceptance test, verified behavior and reproduction commands.

An ignored live preflight test initializes the official runtime in a temporary,
empty profile and checks that session creation requires authentication. It does
not make a model call or access the user's usual Antigravity profile:

```sh
CHAOS_AGY_ACP_PATH=/path/to/agy_acp_server.par \
  cargo test -p acp-guest --test antigravity_live -- --ignored --nocapture
```

After explicitly choosing to log in, create a new private dedicated profile and
use the official runtime's OAuth flow (no prompts/model calls are submitted):

```sh
mkdir -m 700 /absolute/path/to/new-acp-profile
cargo run -p acp-guest --example antigravity_login -- \
  /absolute/path/to/agy_acp_server.par /absolute/path/to/new-acp-profile
```

The runtime opens the browser itself. The helper does not import an existing
profile or print OAuth diagnostics. Keep the profile: authentication remains
owned by the official runtime.

Inspect advertised session modes and model/configuration choices after login,
without submitting a prompt:

```sh
cargo run -p acp-guest --example antigravity_inspect -- \
  /absolute/path/to/agy_acp_server.par /absolute/path/to/new-acp-profile
```
