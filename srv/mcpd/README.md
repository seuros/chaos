# chaos-mcpd

MCP server runtime daemon. Manages driver lifecycle — connects to Model Context
Protocol servers, routes tool calls, and exposes the unified tool surface to the
kernel. Drivers attach and detach here without requiring a restart.
