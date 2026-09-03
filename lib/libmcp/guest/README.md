# mcp-guest

MCP client library. Used by any Chaos component that needs to connect to a Model
Context Protocol server as a guest — tool discovery, invocation, and result
handling.

Stateful MCP remains the default. Servers using the MCP `2026-07-28` stateless
interaction model can be connected to with `.stateless()`:

```rust,ignore
let session = mcp_guest::http("https://example.test/mcp")
    .stateless()
    .connect()
    .await?;
```

An `McpSession` owns its runtime task and transport. Calling `disconnect()` is
idempotent: it first requests graceful shutdown, then force-closes the transport
and aborts the runtime if either exceeds its deadline. Stdio transports kill and
reap child processes during forced shutdown, so a configuration refresh cannot
leave superseded MCP server generations running.

Use `.shutdown_timeout(duration)` on the connection builder to bound transport
shutdown for a server with a known termination budget.
