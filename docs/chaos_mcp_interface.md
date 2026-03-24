# Chaos MCP Server Interface

This document describes the Chaos MCP server interface: a JSON-RPC API that runs over the Model Context Protocol (MCP) transport to control a local Chaos engine.

- Server command: `chaos mcp serve`
- Transport: standard MCP over stdio (JSON-RPC 2.0, line-delimited)

## Starting the server

```bash
chaos mcp serve | your_mcp_client
```

For inspection:

```bash
npx @mcpjam/inspector@latest chaos mcp serve
```

## Configuration

MCP servers can be configured in two ways:

### `.mcp.json` (recommended, cross-harness)

Place a `.mcp.json` in your project root. This format is understood by Chaos, Claude Code, and other MCP-compatible harnesses:

```json
{
  "mcpServers": {
    "my-server": {
      "command": "node",
      "args": ["server.js"],
      "env": {
        "API_KEY": "..."
      }
    }
  }
}
```

### `config.toml` (Chaos-specific)

Use `chaos mcp` to manage MCP servers in `~/.codex/config.toml`.

## Overview

Chaos exposes MCP-compatible methods to manage threads, turns, config, and approvals. The types live in `protocol/src/protocol.rs` and `protocol/src/api/`.

Primary RPCs:
- `thread/start`, `thread/resume`, `thread/fork`, `thread/read`, `thread/list`
- `turn/start`, `turn/steer`, `turn/interrupt`
- `config/read`, `config/value/write`, `config/batchWrite`
- `model/list`

Notifications:
- `thread/started`, `turn/completed`
- `codex/event/*` stream notifications for live agent events

## Threads and turns

Use the thread and turn APIs for all integrations. `thread/start` creates a thread, `turn/start` submits user input, `turn/interrupt` stops an in-flight turn, and `thread/list` / `thread/read` expose persisted history.

## Models

Fetch available models with `model/list`. Supports pagination via `limit` and `cursor`.

Each model includes:
- `id`, `model`, `displayName`, `description`
- `supportedReasoningEfforts` — array of effort levels
- `defaultReasoningEffort` — suggested default
- `inputModalities` — accepted input types
- `isDefault` — recommended for most users

## Event stream

While a conversation runs, the server sends `codex/event` notifications with the serialized event payload matching `core/src/protocol.rs`'s `Event` and `EventMsg` types.

## Tool responses

The `codex` and `codex-reply` tools return standard MCP `CallToolResult` payloads with `structuredContent`:

```json
{
  "content": [{ "type": "text", "text": "Hello from Chaos" }],
  "structuredContent": {
    "threadId": "019bbed6-1e9e-7f31-984c-a05b65045719",
    "content": "Hello from Chaos"
  }
}
```

## Approvals

When Chaos needs approval to apply changes or run commands, the server sends an MCP `elicitation/create` request. The client replies with:

```json
{
  "action": "accept" | "decline" | "cancel",
  "content": {}
}
```

## Stability

Method names, fields, and event shapes may evolve. For authoritative schemas, consult `protocol/src/api/` and the server implementation in `mcp-server/`.
