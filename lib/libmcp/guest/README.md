# mcp-guest

Async Rust client for Model Context Protocol servers, with tool discovery,
invocation, and result handling over stdio or HTTP. Uses Tokio and can be used
independently of Chaos.

## Installation

Requires Rust 1.98 or newer.

```toml
[dependencies]
mcp-guest = "0.12.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The default `stdio` feature connects to local server processes. Enable `http`
for remote servers, or use `default-features = false, features = ["http"]` for
HTTP-only clients.

HTTPS needs a rustls crypto provider, and the crate forces none: enable
`tls-aws-lc` or `tls-ring` (each implies `http`), or rely on a provider your
application already compiles into rama and installs as rustls' process
default. Applications that standardised on one provider build only that one.

```toml
mcp-guest = { version = "0.12.0", default-features = false, features = ["tls-aws-lc"] }
```

```rust,no_run
# #[cfg(feature = "stdio")]
# async fn example() -> Result<(), mcp_guest::GuestError> {
let session = mcp_guest::stdio("my-mcp-server", &[]).connect().await?;
for tool in session.list_tools().await? {
    println!("{}", tool.name);
}
session.disconnect().await?;
# Ok(())
# }
```

## Sessions

With the `http` feature enabled, connect to a remote MCP server:

```rust,no_run
# #[cfg(feature = "http")]
# async fn example() -> Result<(), mcp_guest::GuestError> {
let session = mcp_guest::http("https://example.test/mcp")
    .connect()
    .await?;
session.disconnect().await?;
# Ok(())
# }
```

An `McpSession` owns its runtime task and transport. Calling `disconnect()` is
idempotent: it first requests graceful shutdown, then force-closes the transport
and aborts the runtime if either exceeds its deadline. Stdio transports kill and
reap child processes during forced shutdown, so a configuration refresh cannot
leave superseded MCP server generations running.

Use `.shutdown_timeout(duration)` on the stdio connection builder to bound transport
shutdown for a server with a known termination budget.

## MCP Apps (`ui://`)

`mcp-host` 0.6 MCP Apps views use `ui://` resource URIs and the MIME type
`text/html;profile=mcp-app`. `ToolInfo::ui()` parses tool `_meta.ui` links and
model/app visibility. `list_ui_resources()`, `read_ui_resource(uri)`, and
`read_tool_ui(&tool)` discover and read views through the same MCP session.
Tool-linked views do not have to appear in `resources/list`.

```rust,no_run
# #[cfg(feature = "stdio")]
# async fn example() -> Result<(), mcp_guest::GuestError> {
let session = mcp_guest::stdio("my-mcp-server", &[]).connect().await?;
// Cache listing metadata for views that omit contents-level `_meta.ui`.
session.list_ui_resources().await?;
for tool in session.list_tools().await? {
    if let Some(view) = session.read_tool_ui(&tool).await? {
        let html = view.html()?; // Text or base64-encoded UTF-8 HTML.
        // Pass html and view.ui to your sandboxed renderer.
        // Original contents-level metadata remains in view.contents.
        let _ = html;
    }
}
session.disconnect().await?;
# Ok(())
# }
```

Content-level `_meta.ui` replaces cached listing metadata, rather than merging
permissions. CSP origins are validated; missing policy means no allowed
external origins or browser permissions. Malformed metadata is an error.

The client does **not** render HTML, implement the iframe bridge, or grant
permissions. Only applications that provide a sandboxed MCP Apps renderer
should call `.enable_mcp_apps()` on either connection builder (or
`ClientCapabilities::with_mcp_apps()`). This advertises
`capabilities.extensions["io.modelcontextprotocol/ui"].mimeTypes` during
initialization. Apps support is off by default; raw server declarations alone
never enable it. Once enabled, hosts may expose app-only tools: embedding
applications must use `ToolUi::is_visible_to(UiVisibility::Model)` when building
the model's tool catalog.

## Error classification

`GuestError` exposes `is_retryable()`, `is_timeout()`, and `retry_after()` as
inherent methods. It does not depend on Chaos or implement the private
`chaos_abi::WireFormatError` trait.

## License

Apache-2.0.
