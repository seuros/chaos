# Antigravity OAuth: provider restrictions and clamp design

**Last reviewed:** 2026-08-11

## Status and warning

The Antigravity clamp is experimental and opt-in.

Google's current Antigravity terms prohibit using third-party software, tools,
or services to access Antigravity, explicitly naming use of Antigravity OAuth
from a third-party product as an example. The terms say this may be grounds for
account suspension or termination:

- <https://antigravity.google/terms>

Google has enforced this policy. On 2026-02-27, a Gemini CLI maintainer
confirmed that Google had banned accounts for using third-party tools or proxies
to access Antigravity resources and quotas. Because enforcement happened at a
shared backend layer, affected users also lost access to Gemini CLI and Gemini
Code Assist. The announcement described reinstatement after recertification for
a first flag and a permanent ban after a second violation:

- <https://github.com/google-gemini/gemini-cli/discussions/20632>
- <https://x.com/antigravity/status/2027435365275967591>

Using the official `agy` executable as a subprocess does **not** make a
Chaos-driven workflow officially supported or compliant with those terms.
Operators must decide whether to accept this provider-policy risk. A metered
Gemini API or another provider-supported integration remains the appropriate
choice when terms compliance or account continuity is required.

The strongest official evidence establishes restrictions affecting
Antigravity, Gemini CLI, Gemini Code Assist, and related AI developer services.
It does not establish that every enforcement action disables Gmail, Drive, or
an entire Google identity. The Antigravity terms nevertheless use the broader
word “account,” so Chaos must not promise that enforcement will remain limited
to developer services.

## Why Chaos does not consume Antigravity OAuth credentials directly

Chaos deliberately does not:

- read or parse Antigravity OAuth token files;
- accept browser return codes or refresh tokens;
- copy credentials into Chaos configuration or a service database;
- submit those credentials to Google's private backend;
- reproduce or impersonate Antigravity's private network protocol.

Directly reusing OAuth credentials would create both a security liability and
the exact “piggybacking” pattern called out in Google's enforcement statement.
It would also couple Chaos to an undocumented private API that Google may change
without notice.

Instead, authentication, token refresh, and Google network requests remain
inside the official `agy` process and its dedicated private home.

This separation reduces credential exposure and avoids implementing a private
Google client. It is a technical safety boundary, not a claim of provider
approval.

## Why model-only subprocess output is insufficient

A functional Chaos model session needs more than authenticated model text. It
must retain:

- Chaos-owned tools;
- permission and approval policy;
- sandboxing;
- hooks and lifecycle events;
- usage telemetry;
- process identity and resume behavior.

An earlier proof of concept could send prompts through OAuth-authenticated
`agy`, return text, and resume the provider conversation. It could not use Chaos
tools and was therefore not a usable Chaos transport.

Replacing Chaos with `agy` is also not sufficient. Antigravity does not own the
Chaos trigger lifecycle, session identity, tool policy, event protocol, or
hosted-session orchestration.

## Implemented architecture

```text
Chaos model session
  |
  +-- creates the existing session-scoped Chaos MCP bridge
  |     +-- Unix socket
  |     +-- random capability token
  |
  +-- writes non-secret managed agy configuration
  |     +-- only the `chaos` MCP server
  |     +-- allow `mcp(chaos/*)`
  |     +-- deny native command/filesystem/URL operations
  |
  +-- launches the official agy executable
        +-- owns Google OAuth and network traffic
        +-- inherits the ephemeral bridge socket/token
        +-- invokes `chaos clamp-session-bridge` over stdio
              +-- lists real session tools
              +-- executes through Chaos policy
              +-- returns results to Gemini
```

The bridge socket and capability token exist only in the invocation
environment. They are not written to Antigravity settings, MCP configuration,
conversation state, logs, or service storage.

Every turn runs `agy` with:

- sandbox mode enabled;
- no permission auto-approval flag;
- native command, filesystem, unsandboxed, and URL operations denied;
- only `mcp(chaos/*)` allowed;
- `GEMINI_API_KEY` and `GOOGLE_API_KEY` removed.

Removing metered API-key variables is a fail-closed billing control: failed
subscription authentication must fail the turn rather than silently charge an
API key.

## Dedicated home requirement

`CHAOS_AGY_HOME` is mandatory and must point to a dedicated private directory.
Chaos owns the effective MCP server list and permission policy within this
home, while `agy` owns its OAuth state.

Chaos does not fall back to the user's ordinary `HOME`. This prevents managed
MCP and permission configuration from modifying a non-dedicated Antigravity
installation accidentally.

As with Claude Code, the official provider CLI owns login, token refresh,
account selection, and logout. Chaos does not expose a separate Antigravity
account-management or clamp-lifecycle command namespace.

The home must persist across operating-system processes when provider
conversation resume is required.

Provider conversation state may persist across turns, but the Chaos bridge does
not. Every invocation recreates the managed MCP configuration, permission
policy, Unix socket, and random capability token. A resumed provider
conversation must never retain a stale bridge endpoint or reusable capability.

## Permission boundary

Prompt instructions tell Gemini to use the Chaos MCP server as its sole action
surface, but prompt text is not treated as enforcement.

Live testing showed that a model may still attempt a native Antigravity tool
despite explicit instructions. The configured deny rules blocked that attempt.
The permission policy—not model cooperation—is therefore the security
boundary.

`--dangerously-skip-permissions` must not be added. Doing so would give
Antigravity's native tools authority independent of Chaos and defeat the clamp.

The default Antigravity agent is used because it discovers dynamically
configured MCP tools. The model-only proof of concept declared `tools: []`,
which prevented usable MCP calls even though Antigravity's initialization output
could still list tool names. Declaring `call_mcp_tool` directly in custom-agent
frontmatter also failed in the tested `agy` version. Custom agents should not be
introduced without a live tool round-trip proving that their configuration
preserves dynamic MCP access.

## Verified behavior

Live testing on macOS arm64 with consumer OAuth and `agy 1.1.12` demonstrated:

1. a fresh `chaos exec` turn calling the real Chaos `read_file` tool and using
   file-only information in its answer;
2. a new operating-system process resuming the same Chaos and provider
   conversation and calling `read_file` again;
3. a resumed turn calling the real `exec_command` tool;
4. canonical Chaos command start/completion events;
5. complete invocation and process-cumulative usage telemetry;
6. no direct Gemini API key and no model-only fallback.

These tests establish technical functionality. They do not remove the provider
policy risk described above.

Antigravity tool steps are retained as transport diagnostics. The existing
Chaos MCP bridge already publishes canonical tool and process lifecycle events,
so translating the same `agy` steps into additional Chaos events would
duplicate them. Unknown Antigravity step types remain forward-compatible rather
than failing the turn.

## Approaches intentionally rejected

- **Direct OAuth-token reuse:** exposes credentials and directly matches the
  prohibited third-party OAuth pattern.
- **Calling Google's private backend from Chaos:** undocumented, brittle, and
  indistinguishable from client impersonation.
- **Model-only transport:** authenticates Gemini but omits the Chaos tools and
  lifecycle required for useful operation.
- **Native `agy` tools with auto-approval:** bypasses Chaos permissions,
  sandboxing, hooks, and canonical events.
- **Prompt-only isolation:** the model can ignore instructions; permissions must
  enforce the boundary.
- **Persistent bridge secrets:** turns an invocation-scoped capability into a
  reusable credential.
- **Using `agy` as the primary runtime:** loses Chaos lifecycle and
  orchestration semantics.

## Operational decision

Enable this backend only after the operator has reviewed Google's current terms
and accepted the possibility of losing access to Antigravity and related Gemini
developer services. Do not use an irreplaceable Google identity if that risk is
unacceptable.

For provider-supported operation, use metered Gemini API credentials instead of
the Antigravity clamp.
