# Chaos ACP service

Expose native Chaos conversations to an ACP editor over stdio:

```sh
cargo build -p chaos-cli --no-default-features --bin chaos
/absolute/path/to/chaos --provider YOUR_PROVIDER acp
```

Existing Chaos provider accounts, project trust, instructions, sandbox policy
and permissions apply. `--profile NAME` selects an existing configuration
profile. Stdout is reserved for ACP; diagnostics go to stderr. The service
does not offer interactive ACP login or import editor credentials.

For a Zed custom agent, use an absolute executable path:

```json
{
  "agent_servers": {
    "Chaos": {
      "type": "custom",
      "command": "/absolute/path/to/chaos",
      "args": ["acp"],
      "env": {}
    }
  }
}
```

Select the provider explicitly in `args` if the configured default is not the
intended account. Register only MCP servers already enabled in Chaos with
matching command, arguments and supplied environment. Unregistered editor
MCP declarations are rejected, not executed or persisted.

V1 supports new sessions, continued prompts, text/resource references,
public streamed text and tool previews, once-only approvals and cancellation.
Resource references are untrusted user context, not automatic privileged file
or network reads. Unsaved editor buffers, history reload, model/mode pickers,
images/audio and remote ACP transports are not advertised.

Cancellation keeps the session busy until the native turn and its cancellation
hook have joined. Disconnect performs bounded shutdown. Unconfirmed cleanup
fails closed; uncertain prompts are never automatically replayed.

Before treating an editor setup as validated, check streamed and continued
responses, a real file read, an approved edit, a denied action, cancellation
followed by a fresh turn, session isolation and subprocess exit on close.
Protocol tests alone are not a real-editor acceptance test.
