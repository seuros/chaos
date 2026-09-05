# Child agents and instruction scope

Child agents are bounded workers entrusted with delegated tasks. The parent
session keeps responsibility for integrating their work. Child agents need only
the context required for their task; they need not inherit the parent's identity.

## Instructions

Use two distinct settings in `config.toml`:

```toml
# Applied to this session as a developer message.
developer_instructions = "Explain consequential actions before taking them."

# Kept as configuration in the parent, but not sent to the parent's model.
# Applied to delegated children in addition to their selected role.
child_instructions = """
You are a child agent working on a bounded task.
Keep authorship clear and preserve work you did not create.
"""
```

`child_instructions` is optional. It is included only in the initial developer
context of delegated process-spawn sessions, including nested children. The same
scope check applies when initial context is rebuilt. CLI, API, MCP and other main
sessions do not receive it. Internal reviewers, compactors and memory workers
keep their own instruction contracts instead.

Agent-role Markdown bodies and TOML `developer_instructions` select the child's
role; they do not replace project `child_instructions`. Reviewer personas and
collaboration modes also use `developer_instructions`, independently of the
child-only setting. A role configuration can explicitly override
`child_instructions` through normal configuration precedence.

These settings control instruction placement, not context size. Use
`fork_context = false` (the default) for a lightweight child and provide bounded
task context. Explicit forks intentionally carry conversation history; this
setting does not redact that history or remove other inherited configuration.

## Migration

This change renames the previous delegation terminology throughout the source,
configuration, role parser, IPC, tools and UI. It intentionally does not retain
the old names as aliases.

When migrating the previous instruction setting, choose based on its contents:

- Text addressed to children belongs in `child_instructions`.
- General session, persona or collaboration-mode text belongs in
  `developer_instructions`.
- Role TOML files use `developer_instructions`; Markdown role bodies need no
  instruction-key change.

CSV delegation tools are now `spawn_child_agents_on_csv` and
`report_child_agent_job_result`. Update automation and clients using the old tool
names, IPC fields, or progress-message prefix. The naming library is now
`chaos-child-agents`.

Existing job data stays in the same `agent_jobs` and `agent_job_items` tables;
the Rust API rename does not rewrite data or applied database migrations.

Build and install the updated binary, migrate configuration and clients, then
start fresh sessions. Editing configuration does not retract a developer message
already present in an existing session's history.
