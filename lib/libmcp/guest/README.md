# mcp-guest

Async Rust client for Model Context Protocol servers, with tool discovery,
invocation, and result handling over stdio or HTTP. Uses Tokio and can be used
independently of Chaos.

## Installation

Requires Rust 1.98 or newer.

```toml
[dependencies]
mcp-guest = "0.10.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The default `stdio` feature connects to local server processes. Enable `http`
for remote servers, or use `default-features = false, features = ["http"]` for
HTTP-only clients.

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

## Error classification

`GuestError` exposes `is_retryable()`, `is_timeout()`, and `retry_after()` as
inherent methods. It does not depend on Chaos or implement the private
`chaos_abi::WireFormatError` trait.

## Release preparation

This crate is versioned independently of the Chaos workspace. From the
repository root, validate a release with:

```sh
cargo test -p mcp-guest
cargo test -p mcp-guest --no-default-features
cargo test -p mcp-guest --no-default-features --features http
cargo test -p mcp-guest --all-features
cargo publish -p mcp-guest --dry-run --all-features
```

Run the publish dry-run from a clean checkout. Publishing to crates.io is a
separate, explicit release step.

## License

Apache-2.0.
