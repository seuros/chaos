# acp-guest

Bounded Tokio connections to ACP agents, using the official
`agent-client-protocol` SDK for protocol types, dispatch and correlation.

The SDK's `process`, `stdio`, `schemars`, HTTP and RMCP integrations are not
enabled. Tokio owns I/O; the embedding application owns process execution.

## Boundaries

- Negotiates stable ACP v1, without automatically authenticating.
- Denies permission requests by default; an explicit application-owned
  `PermissionHandler` can decide individual requests. Optional services are off
  by default.
- Sending `session/cancel` resolves pending permission requests for that session
  and elicitation requests as cancelled and drops their application-policy futures. Policy futures must
  therefore be cancellation-safe; late approvals cannot escape cancellation.
- Bounds incoming/outgoing JSON lines and unread session updates.
- Closes on update overflow or a dropped request future. Request timeouts return
  an error without closing an established connection; late replies are discarded.
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

## Opt-in client services

`ConnectionOptions` accepts application-owned `FileReader`, `FileWriter`,
`TerminalHandler` and `ElicitationHandler` implementations. Read/write
capabilities are independent; terminal support requires all five lifecycle
methods. Elicitation advertises only the handler's form/URL modes.
Handlers run concurrently outside protocol dispatch, with bounded admission
and deadlines. No disk fallback, shell execution or URL opening is installed.

The application authorizes session/request identities, canonical paths and
terminal handles, enforces execution/secret policy and owns process cleanup.
Use per-connection policy/UI adapters bound to the verified agent identity;
neither a session ID nor the peer's `agentInfo` authenticates a caller. Do not
share an unscoped service handler across different agents or users.
Form UIs must never request credentials and must allow review/edit, decline and
cancel before submission. URL UIs must show the full URL and host, obtain
consent and open a secure context inaccessible to the agent/model. The library
ignores unknown/already-completed URL IDs; consent is not external completion.
Terminal output must respect UTF-8 byte limits. Futures must be cancellation-safe:
a timed-out write or terminal creation may have taken effect, and disconnect
does not prove processes stopped. Terminal owners must kill, release and reap
their resources even when an individual request future is dropped.

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
