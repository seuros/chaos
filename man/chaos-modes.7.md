# chaos-modes(7)

## NAME

chaos-modes - define and switch session-scoped FreeChaOS collaboration modes

## DESCRIPTION

FreeChaOS modes are session-scoped instruction and capability profiles. A mode
selects the developer instructions, reasoning effort, and model-visible tool
surface used for subsequent samples in one process.

Every installation includes the `default` and `plan` modes. Additional modes
can be installed as Markdown files without rebuilding the kernel.

Mode state is isolated per process. Switching a root session changes its TUI
mode indicator. Switching a minion changes only that minion and does not change
the parent or sibling sessions.

## BUILT-IN MODES

| Mode | Description |
|------|-------------|
| `default` | General-purpose execution mode with mutation and implementation planning enabled |
| `plan` | Conversational planning mode with repository mutation and `update_plan` disabled |

Plan mode retains tools used for inspection, builds, tests, structured
questions, and mode switching. It removes `apply_patch`, `update_plan`,
`request_permissions`, dynamic mutating tools, and MCP tools marked
destructive.

## DISCOVERY

Models discover the active mode and caller-visible catalog by reading:

```text
chaos://modes
```

The resource is caller-filtered. A child can see fewer modes than its parent,
and a fixed-mode child does not receive the `switch_mode` tool. The resource
returns mode metadata and capabilities but never returns custom mode
instructions.

The installation-level MCP resource reports the default root catalog. A live
session can expose a narrower catalog and a different active mode.

## SWITCHING

When mode switching is enabled, the model calls:

```json
{
  "mode_id": "plan"
}
```

The kernel updates the session policy, records the new developer instructions,
and rebuilds the effective tool surface before the next model sample. This also
applies to the immediate follow-up sample in the same user turn.

Switching to the already active mode is a no-op.

## CHILD SESSIONS

The orchestrator controls a child's initial mode, allowed modes, and ability to
switch. A child cannot enable a mode or capability unavailable in the active
parent mode.

Create a fixed planning minion by selecting `plan` and omitting
`allowed_modes`. Create a switchable child with an explicit catalog:

```json
{
  "mode": "default",
  "allowed_modes": ["default", "plan"],
  "allow_mode_switching": true
}
```

A minion can switch its own mode when permitted, but its switch does not update
the root session's TUI.

## CUSTOM MODES

Place custom mode files in `${CHAOS_HOME}/modes/`. Each Markdown file begins
with TOML front matter delimited by `+++`; the remaining content is injected as
developer instructions whenever the mode is active.

```markdown
+++
id = "research"
title = "Research"
description = "Evidence-first investigation."
reasoning_effort = "high"

[capabilities]
mutation = false
request_user_input = true
update_plan = true
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
| `update_plan` | Permit the implementation checklist tool |

A mode can remove capabilities but cannot grant filesystem, network, sandbox,
approval, delegation, or other security authority.

## FILES

- `${CHAOS_HOME}/modes/*.md` - custom collaboration mode definitions
- `~/.chaos/modes/*.md` - default custom mode location

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-support.7](./chaos-support.7.md)
