+++
title = "chaos-modes(7)"
summary = "Collaboration mode discovery and switching."
+++

# chaos-modes(7)

## NAME

chaos-modes - define and switch session-scoped FreeChaOS collaboration modes

## DESCRIPTION

Modes select instructions, reasoning effort, and available tools for a session.

Every installation includes the `default` and `plan` modes. Additional modes
can be imported as Markdown documents.

Mode state is isolated per process. Switching a root session changes its TUI
mode indicator. Switching a minion changes only that minion and does not change
the parent or sibling sessions.

## BUILT-IN MODES

| Mode | Description |
|------|-------------|
| `default` | Execution mode; compact progress tools appear only for an explicitly attached plan |
| `plan` | Conversational planning with durable planning-record writes permitted, repository mutation disabled |

Plan mode retains tools used for inspection, builds, tests, structured
questions, and mode switching. It removes `apply_patch`,
`request_permissions`, dynamic mutating tools, and MCP tools marked
destructive.

To revise an attached plan, switch to Plan mode and return to execution afterward;
no operator mode switch is needed. See [chaos-planning(7)](./chaos-planning.7.md)
for durable plans and backend-specific features.

## DISCOVERY

Read `chaos://modes` for the active mode and allowed choices, not mode instructions.
A child may have fewer choices than its parent.

The installation-level MCP resource reports the default root catalog. A live
session can expose a narrower catalog and a different active mode.

## SWITCHING

Use `switch_mode` when available. The new mode applies to the next model request,
including within the same turn. Selecting the current mode changes nothing.

## CHILD SESSIONS

The orchestrator controls a child's initial mode, allowed modes, and ability to
switch. A child cannot enable a mode or capability unavailable in the active
parent mode.

For a fixed planning child, select `plan` without a switchable catalog.
For a child that must alternate planning and execution, allow both modes and
enable switching. Delegation still requires explicit user authorization.

## CUSTOM MODES

Custom modes are Markdown documents stored in the runtime database and
managed with `chaos modes`:

```sh
chaos modes import research.md      # stored under the file stem; must equal `id`
chaos modes list
chaos modes show research
chaos modes export research > research.md
chaos modes disable research
chaos modes remove research
```

Each document begins with TOML front matter delimited by `+++`; the remaining
content is injected as developer instructions whenever the mode is active.
Import validates the document. A stored mode that no longer parses is skipped
with a warning at session start. `${CHAOS_HOME}/modes/` is no longer read.

```markdown
+++
id = "research"
title = "Research"
description = "Evidence-first investigation."
reasoning_effort = "high"

[capabilities]
mutation = false
request_user_input = true
planning_records = true
+++
Investigate the request using available read-only tools. Separate verified
facts from inferences and report uncertainty explicitly.
```

Mode IDs must start with a lowercase ASCII letter or digit. Remaining
characters can be lowercase ASCII letters, digits, `-`, or `_`. The IDs
`default` and `plan` are reserved.

## CAPABILITIES

Capabilities default to enabled. A custom mode can set a capability to `false`
to narrow its model-visible surface:

| Capability | Effect |
|------------|--------|
| `mutation` | Permit tools that can change user or repository state |
| `request_user_input` | Permit the structured question tool |
| `planning_records` | Permit database planning operations, independently of repository mutation |

A mode can remove capabilities but cannot grant filesystem, network, sandbox,
approval, delegation, or other security authority.

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-support.7](./chaos-support.7.md)
