# Chaos ACP service

Expose native Chaos conversations to an ACP editor over stdio:

```sh
cargo build -p chaos-cli -p chaos_journald --no-default-features --bins
/absolute/path/to/chaos --provider YOUR_PROVIDER acp
```

Use the CLI and journal helper from the same build. When upgrading a development
checkout, restart any older journal daemon before editor testing; unknown ACP
provenance fails closed rather than returning a supposedly durable session.

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
public streamed text and tool previews, once-only approvals and cancellation,
model/mode selectors, and durable session list/load/resume.
Resource references are untrusted user context, not automatic privileged file
or network reads. Model choices stay within the selected provider's cached
catalog; mode choices obey the current native policy. Controls are idle-only.
Images/audio, interactive login, editor terminals and remote ACP transports
are not advertised.

Slash commands are advertised with `available_commands_update` on new/load/
resume and refreshed with prompts. Send them as ordinary `session/prompt`
text: `/help`, `/status`, `/model [id]`, `/mode [id]` (`/collab`),
`/plan [task]`, `/compact`, `/review [instructions]`, `/rename <name>` and
enabled `/prompts:<name>` documents. Model/mode changes obey the same native
policy as ACP selectors. Custom prompt arguments use the console's named
(`KEY="value"`) and positional quoting/expansion rules, without importing the
terminal UI. Unknown/unsupported commands and argument errors return a local
message, never an LLM interpretation. UI commands such as `/quit`, `/theme`,
`/permissions` and account pickers are not proxied.

ACP session IDs are native journal UUIDs. New-session success confirms native
metadata and controls before returning the ID. Only typed ACP-owned sessions
in the selected provider/runtime database are restorable; old generic API
sessions are not inferred to be ACP sessions. Restore rechecks the canonical
workspace, current trust/configuration, registered MCP declarations, model/mode
policy and exclusive native writer ownership. Load replays only public text
and bounded tool previews; resume does not replay. Neither reexecutes old work,
restores approval grants, or revives prior background/goal executors. An
unfinished foreground turn is rejected because its execution outcome is unknown.

When the editor negotiates text reads, foreground `read_file` reads unsaved
editor text through the usual slice/indentation formatting. Files must already
exist as regular files and pass native lexical/canonical path and VFS checks.
With text writes negotiated, `apply_patch` supports one existing-file text
update or one file creation after native admission, mode checks and any
required approval. Creation requires an existing parent directory, an absent
native target (including no dangling symlink), and an explicit ACP
resource-not-found response from the editor. A generic read error is not
absence; even an empty unsaved buffer conflicts. Parent identity and absence
are rechecked after approval. Delete, rename, multi-file actions and shell-intercepted patches are rejected
in editor mode. Read-only clients receive no write requests. Other shell tools
still have their ordinary native disk semantics; this is not a virtual filesystem.

Editor text is untrusted tool data, not configuration or instructions. Requests
are session/foreground-turn scoped, byte-limited and have finite deadlines.
After selecting editor state, errors never fall back to native disk writes.
Chaos sends no separate save request. ACP's write method delegates to the
client filesystem, so client-controlled persistence may save the file; a
buffer-only/no-save guarantee is not part of the protocol.
Before an approved write, file identity/policy and current editor text
are rechecked; an observed conflict rejects the write. ACP provides no atomic
revision/CAS or create-exclusive operation, so typing or filesystem changes
after that last check remain a race, including a newly appeared file. An
unacknowledged sent write invalidates the session with an
unknown outcome and is never retried.

Cancellation keeps the session busy until the native turn and its cancellation
hook have joined. Disconnect performs bounded shutdown. Unconfirmed cleanup
fails closed; uncertain prompts are never automatically replayed.

Before treating an editor setup as validated, check streamed and continued
responses, a real file read, an approved edit, a denied action, cancellation
followed by a fresh turn, session isolation and subprocess exit on close.
Also check model/mode changes, restoration after restarting the subprocess,
an unsaved read differing from disk, an approved editor edit (including the
client's save behavior), and rejection or cancellation without a disk fallback.
Protocol tests alone are not a real-editor acceptance test.
