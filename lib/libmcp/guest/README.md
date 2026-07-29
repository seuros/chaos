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
