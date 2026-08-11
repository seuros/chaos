# Antigravity full-Chaos clamp plan

**Date:** 2026-08-11
**Branch:** `feat/antigravity-clamp`
**Issue:** #26
**Withdrawn proof-of-concept:** #25
**Status:** real Chaos bridge proven for fresh and cross-process resumed turns;
implementation and regression review in progress

## Purpose

The Antigravity clamp is useful only if a Gemini resident retains the practical
agency it has on the direct Chaos provider path. Successful OAuth-authenticated
model output and provider-conversation resume are necessary but not sufficient.

The acceptance criterion is:

> A subscription-authenticated `agy` process can perform an iterative model
> turn using the real tools, permissions, events, and session lifecycle owned
> by Chaos, without direct use of Antigravity OAuth credentials by Chaos and
> without granting Antigravity's native tools independent authority.

Do not reopen a pull request while the backend remains model-only.

## Existing branch foundation

Commit `70048a3ec` currently provides:

- the `AntigravityTransport` subprocess wrapper;
- Google consumer OAuth lifecycle commands delegated to the official `agy`
  binary;
- isolated `CHAOS_AGY_HOME` state;
- removal of `GEMINI_API_KEY` and `GOOGLE_API_KEY`;
- model and effort mapping;
- JSONL parsing and normalized final response/usage;
- provider-conversation resume across separate Chaos processes;
- fake-executable integration tests and an ignored live smoke test.

These pieces should be retained where they still fit. They are not by
themselves evidence of a usable Souls.house resident transport.

## Proven MCP spike

The runnable development evidence lives outside Git worktrees at:

```text
~/.local/share/mira-antigravity-spike
```

Do not move OAuth state, browser return codes, or tokens into the repository.

Tested artifacts:

- `agy 1.1.11` for the original standalone MCP spike
- `agy 1.1.12` for environment inheritance and full-Chaos tests
- macOS arm64
- Google consumer OAuth account
- `gemini-3.1-pro-low`

On 2026-08-11, print-mode `agy` successfully:

1. loaded an MCP server;
2. exposed its tool to the model;
3. requested that tool during an agent turn;
4. passed the MCP arguments to the server;
5. received the result;
6. continued the model turn with the result;
7. emitted tool start/completion JSONL steps and a final response.

The successful invocation used:

- `--sandbox`;
- no `--dangerously-skip-permissions`;
- an exact allow rule for `mcp(chaos-spike/*)`;
- explicit deny rules for native commands, filesystem access, unsandboxed
  execution, and URL access.

The evidence files are:

```text
mcp-bridge-smoke-6.jsonl
mcp-echo-calls.jsonl
home/.gemini/antigravity-cli/settings.json
```

The returned marker was:

```text
CHAOS_TOOL_CONFIRMED:AGY_CHAOS_MCP_OK
```

This proves MCP feasibility through the official OAuth-authenticated `agy`
process. It does not yet prove that the existing Chaos bridge works under
`agy`, that every Chaos tool schema is compatible, or that normalized Chaos
events remain correct.

## Intended architecture

```text
Chaos ModelClientSession
  |
  +-- ensure_clamp_mcp_bridge()
  |     |
  |     +-- session-scoped Unix socket
  |     +-- random capability token
  |
  +-- write managed, non-secret agy configuration
  |
  +-- launch official agy
        |
        +-- Google consumer OAuth remains owned by agy
        +-- CHAOS_CLAMP_MCP_SOCKET in process environment
        +-- CHAOS_CLAMP_MCP_TOKEN in process environment
        +-- native tool permissions denied
        +-- only mcp(chaos/*) allowed
        |
        +-- spawn `chaos clamp-session-bridge`
              |
              +-- list real session tools
              +-- execute them through Chaos
              +-- return tool results to Gemini
```

The existing `clamp-session-bridge` and session socket are the preferred seam.
Do not create a second Antigravity-specific tool execution protocol unless the
existing bridge is proven incompatible.

## Implementation sequence

### 1. Make Antigravity configuration managed and non-secret

Add a small configuration preparation layer to `chaos-clamp` or the kernel
integration that writes only files owned by Chaos inside the dedicated
`CHAOS_AGY_HOME`:

- MCP configuration naming a `chaos` server whose command is the current Chaos
  executable and whose arguments are `clamp-session-bridge`;
- `agy` permission settings that allow only `mcp(chaos/*)` and deny native
  command, filesystem, unsandboxed, and URL operations.

Requirements:

- do not write the bridge socket path or capability token to persistent config;
- pass both through the parent `agy` environment so the MCP child inherits
  them;
- write files atomically with private permissions;
- do not overwrite unrelated state in a non-dedicated home;
- either require a dedicated managed home or merge only the fields Chaos owns;
- validate effective configuration before launching `agy`;
- fail closed if safe configuration cannot be established.

First prove that `agy`-spawned stdio MCP children inherit the bridge
environment. If they do not, use an ephemeral per-invocation config file or
wrapper directory that is deleted after the turn. Do not persist the token.

### 2. Reuse the session bridge in `stream_antigravity`

Before launching an Antigravity turn:

1. call `ensure_clamp_mcp_bridge()`;
2. obtain the socket path and capability token;
3. pass them into `AntigravityConfig`;
4. export them only to the `agy` process;
5. keep metered Gemini API-key variables removed.

The bridge must expose the same effective session tools as the Claude Code
clamp. Tool execution must continue to use Chaos approval policy, sandboxing,
hooks, and event publication.

### 3. Establish reliable model instructions

The earlier transport-only custom agent declared `tools: []`. That disables the
MCP route and must not be reused.

The default `agy` agent successfully called MCP under the scoped permission
policy. A custom agent with no `tools` field did not reliably use the MCP
surface and attempted native file inspection before becoming stuck.

Proceed in this order:

1. use the default agent plus explicit full-prompt instructions and strict
   permissions for the first real bridge proof;
2. inspect the effective tool description and system behavior;
3. only introduce a custom agent after a live test proves that its declared
   tool configuration includes working dynamic MCP tools;
4. treat permissions, not prompt text, as the security boundary.

The model should be told:

- Chaos MCP tools are its sole action surface;
- native Antigravity tools are unavailable;
- tool results are authoritative;
- it may make multiple MCP calls before answering;
- it should return the user-facing answer without Antigravity checkpoint or
  timestamp boilerplate.

### 4. Normalize tool activity

Extend Antigravity JSONL parsing for observed tool steps:

- `step_type: "tool"`;
- `state: "ACTIVE"`, `"DONE"`, or `"ERROR"`;
- `tool_name`;
- structured parameters;
- output or error.

Do not guess undocumented fields. Unknown step types must remain forward
compatible rather than failing the whole turn.

Determine which tool events the Chaos session bridge already emits. Avoid
duplicating tool start/completion events if bridge execution already publishes
canonical Chaos events. The Antigravity parser may need tool steps mainly for:

- detecting denied native-tool attempts;
- classifying MCP failures;
- proving that the result returned to the model;
- transport diagnostics.

### 5. Preserve resume semantics

Every resumed operating-system process must recreate:

- the MCP configuration;
- scoped permission settings;
- bridge socket and capability environment;
- original model selection;
- provider conversation ID.

The provider conversation must not retain a stale bridge endpoint or capability
token. Tool discovery and connection must be invocation-scoped even when model
context is provider-resumed.

### 6. Update capability reporting

Do not report `chaos_tool_bridge: true` merely because configuration files were
written.

Report full bridge authority only after:

- the bridge is configured;
- the session MCP endpoint is available;
- safe permission policy is active;
- the runtime path has passed an actual tool round trip.

Static `status` output may distinguish:

- CLI/auth available;
- MCP configuration support available;
- runtime session bridge active only during a turn.

## Required tests

### Unit tests

- managed MCP config contains the current Chaos executable and
  `clamp-session-bridge`;
- persistent config contains no bridge token or socket;
- permission settings allow `mcp(chaos/*)`;
- native command, filesystem, unsandboxed, and URL permissions are denied;
- writes are atomic and private;
- existing unrelated OAuth state is preserved;
- API-key environment variables are removed;
- bridge environment reaches the `agy` child;
- tool JSONL steps parse without breaking unknown step types;
- native-tool denial is classified separately from MCP failure.

### Fake-`agy` integration tests

- assert bridge socket/token exist in the `agy` environment;
- inspect generated settings and MCP configuration;
- emit MCP tool start/done/error-shaped JSONL;
- complete a fresh and resumed turn;
- verify secrets do not appear in normalized output.

### Live MCP transport test

Use a deterministic local MCP tool and the authenticated isolated home:

- one tool call;
- multiple sequential tool calls;
- tool error followed by model recovery;
- native filesystem or shell attempt denied;
- final answer derived from the tool result.

### Live full-Chaos test

Run the actual Chaos binary and existing session MCP bridge:

1. start `chaos exec --json` with Antigravity clamping;
2. require Gemini to invoke a harmless deterministic Chaos tool;
3. verify canonical Chaos tool events;
4. verify the final answer contains information available only from the tool;
5. resume the same Chaos process from a new OS process;
6. require another tool call;
7. verify provider context and tool access both survived.

This is the minimum proof required before reopening a PR.

## Implementation checkpoint — 2026-08-11

The first full implementation pass now:

- reuses the existing session-scoped `ClampSessionBridge`;
- writes managed `mcp_config.json` and Antigravity permission settings inside
  the dedicated `CHAOS_AGY_HOME`;
- exposes only the `chaos` MCP server;
- denies native command, filesystem, unsandboxed, and URL operations;
- passes the bridge socket and capability token only through the `agy`
  environment;
- uses the default Antigravity agent rather than the dead-end transport-only
  custom agent; the public transport configuration no longer permits callers
  to select a custom agent and accidentally remove dynamic MCP tools;
- removes metered Gemini API-key variables;
- preserves provider conversation resume and normalized usage.

Live proofs are stored outside Git worktrees. The environment inheritance proof
is:

```text
~/.local/share/mira-antigravity-spike/mcp-env-inheritance.jsonl
```

The full-Chaos proofs are under:

```text
~/.local/share/mira-antigravity-spike/full-chaos-smoke
```

The following passed against OAuth-authenticated `agy 1.1.12`:

1. `mcp-env-inheritance.jsonl` proves an `agy`-spawned MCP child inherited a
   random parent-only environment marker and returned it to Gemini.
2. `fresh.jsonl` proves a fresh `chaos exec` turn called the real Chaos
   `read_file` tool and returned a file-only marker.
3. `resume.jsonl` proves a new operating-system process resumed the same Chaos
   and provider conversation, recreated tool access, and called `read_file`
   again.
4. `exec-tool.jsonl` proves a resumed turn called the real Chaos
   `exec_command` tool and emitted canonical `item.started` and
   `item.completed` command events before the final answer.
5. All three full-Chaos turns emitted complete invocation and
   process-cumulative usage without a `turn.failed` event.

The `read_file` smoke turn did not emit a generic process item because Chaos
does not currently publish one for that built-in tool on any provider path.
The `exec_command` turn was used to verify canonical tool lifecycle events.

Remaining before a new PR:

- review the full branch diff against `master`;
- split the implementation into atomic signed-off commits;
- keep the PR withdrawn until those checks pass.

Focused automated validation completed on 2026-08-11:

```text
cargo test -q -p chaos-clamp
cargo test -q -p chaos-cli --test clamp_antigravity
cargo check -q -p chaos-kern
```

Results:

- `chaos-clamp`: 30 passed, 0 failed; live provider tests remained ignored;
- `clamp_antigravity`: 5 passed, 0 failed;
- `chaos-kern`: check passed.

Final safety review removed the lifecycle command's fallback from
`CHAOS_AGY_HOME` to ordinary `HOME`. Connect and disconnect now require an
explicit dedicated home, matching turn execution and preventing managed
Antigravity configuration or credential removal from touching a user's normal
home accidentally. A regression test proves connect fails closed without the
environment variable.

Repository-wide contribution checks:

```text
just fmt
cargo fmt --all -- --check
just test
```

Formatting passed. The complete test run executed 2,734 tests: 2,730 passed,
19 were skipped, and 4 failed for confirmed baseline/environment reasons:

1. `review_diff_prefetch_disables_external_diff_helpers` and
   `review_diff_prefetch_disables_textconv_helpers` receive ANSI-colored Git
   output because Daniel's global Git configuration sets `color.ui=always`;
   their assertions expect literal uncolored `-before\n` and `+after\n`.
2. `undo_restores_untracked_file_edit` likewise expects literal uncolored
   `git status --short` output and misses the ANSI-wrapped `?? notes.txt`.
3. `unified_exec_emits_one_begin_and_one_end_event` reproduces identically when
   run alone on the clean `master` checkout at `62ec36cbc`.

The branch does not modify any of those tests or their implementation areas.
Do not contaminate this focused change by fixing baseline test isolation here.

The first focused run caught and corrected a stale model-only authority assertion
in the ignored Antigravity smoke test. The live smoke now requires an explicit
session bridge capability rather than pretending a model-only invocation is a
valid full clamp.

Observed tool-step review:

- successful Chaos MCP calls are `step_type: "tool"`,
  `tool_name: "call_mcp_tool"`, with `ACTIVE` then `DONE`;
- a blocked native `view_file` attempt is `state: "ERROR"` and reports that it
  matched the configured `read_file(...)` deny rule;
- the existing Chaos MCP bridge already emits canonical tool/process events, so
  `agy` tool steps must remain transport diagnostics and must not be translated
  into duplicate Chaos lifecycle events;
- unknown `step_type` values remain parseable because `step_type` is stored as
  a string rather than a closed enum.

### Regression tests

- Claude Code clamp behavior remains unchanged;
- direct Gemini API behavior remains unchanged;
- Anthropic and Gemini API keys never leak into their subscription subprocesses;
- clamp selection remains explicit and never silently falls back to metered API
  transport;
- non-clamped interactive and exec sessions remain unchanged.

## Dead ends and approaches not worth repeating

### Model-only transport

Do not ship a backend that only renders the prompt, returns final text, and
resumes provider conversation state. That creates a chatbot, not a functional
Chaos resident.

### Direct use of Antigravity OAuth credentials

Do not read, copy, parse, export, refresh, or submit Antigravity OAuth tokens
from Chaos or Souls.house. Google authentication and network requests must
remain inside the official `agy` process.

### `--dangerously-skip-permissions`

Do not use it. It grants native Antigravity tools independent authority and
defeats Chaos's permission boundary.

### Trusting prompt instructions as isolation

“Do not use native tools” is not enforcement. The live custom-agent test tried
`view_file` despite explicit instructions. The deny rules correctly blocked it.
Keep the deny policy even if model instructions appear reliable.

### `tools: []` transport agent

This was the original spike configuration. It prevents usable MCP calls while
the `init.tools` catalog can still misleadingly list tool names. Do not infer
tool usability from the init catalog.

### Declaring `call_mcp_tool` directly in custom-agent frontmatter

The attempted `tools: [call_mcp_tool]` configuration failed with “no tool
converter registered.” Do not revisit it without evidence for the correct
dynamic MCP tool declaration syntax in the tested `agy` version.

### Custom agent before the bridge works

A custom agent with no `tools` field attempted native file inspection and did
not complete the MCP call. Use the default agent for the first integration
proof. Optimize agent behavior only after functionality is established.

### Replacing Chaos with `agy`

`agy` does not replace Chaos's trigger lifecycle, session identity, resident
tools, permission semantics, event normalization, or Souls.house orchestration.
Use it as the official Google-authenticated harness beneath Chaos, not as the
resident runtime.

### Antigravity SDK as subscription authentication

The public SDK is useful for programmatic agents but currently documents API
key and Vertex credentials, not the tested consumer subscription OAuth path.
It does not satisfy this work's billing/authentication goal unless Google adds
and documents that capability.

### Persistent per-session MCP secrets

Do not store bridge socket paths or capability tokens in
`mcp_config.json`, Rails, logs, conversation state, or backups. They are
invocation-scoped capabilities.

### Treating successful tool request as sufficient

The following are all separately required:

- tool discovery;
- permission authorization;
- execution by the real Chaos bridge;
- result returned to Gemini;
- Gemini using the result;
- canonical Chaos events;
- cross-process resume;
- native-tool denial.

Tests must prove the chain rather than assert only its final text.

## Commit strategy

Keep commits atomic and signed off:

1. plan and issue reference;
2. managed safe `agy` configuration plus tests;
3. session bridge wiring plus fake integration tests;
4. tool-step normalization and diagnostics;
5. live-test harness and documentation;
6. final review corrections.

Each code commit should compile and pass its focused tests. Before any new PR:

```text
just fmt
just test
```

Review the complete diff as an agent, then let Daniel decide whether it is
ready. Fill the PR template with What / Why / How and reference issue #26.

## Continuation checkpoint

If context is compacted, resume here:

1. read this file;
2. inspect issue #26 and closed PR #25;
3. inspect the uncommitted worktree diff and external full-Chaos evidence under
   `~/.local/share/mira-antigravity-spike/full-chaos-smoke`;
4. do not return to model-only work;
5. the next validation command is `just fmt`, followed by `just test`;
6. formatting and the complete suite have already run; read the documented
   baseline failures above rather than rediscovering them;
7. review the complete `master...HEAD` plus worktree diff, scan for secrets,
   then create atomic
   signed-off commits;
8. do not reopen a PR until Daniel has reviewed the finished diff.
