# chaos-mcp-runtime

MCP session runtime with kernel-facing types. Bridges the guest protocol to the
kernel's internal representation of tool calls and responses.

## Private Skynet fleet extension

Only `notifications/skynet/fleet/inbox` requests an autonomous inbox wake.
Params must contain exactly `uri: "skynet://fleet/inbox"` and `message_ids`
(1–50 canonical positive decimal signed-bigint strings); no message content.
The configured MCP entry name is preserved, not assumed to be `skynet`.
Ordinary resource-update notifications retain their existing non-waking behavior.

The kernel coalesces IDs by configured server and URI in its existing background
task journal. Its single session runner admits an idle continuation, or waits
for the current turn to finish. Fleet hints are delivered only before a
continuation's first sample, never through active-turn input injection. Restore
loads delivery markers before consuming reconnect hints. The recipient reads
the exact server/URI, treats peer content as untrusted requests under existing
local permissions, and uses the server's native fleet tool to acknowledge only
after handling. Host delivery markers are not remote acknowledgements.

This is durable wake admission/deduplication, **not exactly-once execution**.
Full/closed notification queues, unsupported clients/tools, interrupted wake
policy, journal failures, and ambiguous crashed continuations do not acknowledge
the server inbox. The server must retain unread messages and replay hints on
reconnect; crash-ambiguous continuations require owner reconciliation rather
than automatic side-effect replay. Failed handling is not automatically retried
by repeating an already-delivered ID in the same saved conversation. Dedup state
is conversation-scoped (not preserved across forks), and grows with admitted
IDs. No daemon is started for an unloaded session.

The client-only request `skynet/fleet/hostInfo` returns exactly
`{os, arch, capabilities, restrictions}` from harness-owned state. OS/architecture
use Rust platform constants. Capabilities are currently an empty array (no
executable probing); restrictions contain the current MCP-handler approval
policy, if accessible. Sandbox facets are omitted because this handler does not
own them. Request params are ignored, and no model tool exposes this request.
The response is information, never permission authority; it contains no machine
address, user identity, paths, environment dump, or credentials.
