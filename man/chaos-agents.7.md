+++
title = "chaos-agents(7)"
summary = "Standalone agent role definitions."
+++

# chaos-agents(7)

## NAME

chaos-agents - standalone agent role definitions

## ROLE DOCUMENTS

A role is a `.toml` document or a Markdown document with TOML front matter.
Your own roles live in the runtime database and are managed with
`chaos agents`; a trusted project ships shared roles as files under
`.chaos/agents/` (scanned recursively).

```sh
chaos agents import ~/roles/researcher.toml   # name comes from the document
chaos agents list
chaos agents show researcher
chaos agents export researcher > researcher.toml
chaos agents disable researcher
chaos agents remove researcher
```

Import validates the document and records the file's directory, so relative
config paths keep resolving against it. Changes apply to new sessions.
`$CHAOS_HOME/agents/` and `/etc/chaos/agents/` are no longer read.

Each file must declare a non-empty `name` and non-blank
`developer_instructions`. A Markdown body can supply the instructions instead
of a frontmatter field, but not both. The built-in default role is intentionally
unconstrained and does not require instructions.

```toml
# ~/.chaos/agents/researcher.toml
name = "researcher"
description = "Research code and summarize findings."
nickname_candidates = ["Hypatia", "Noether"]
topics = ["research"]
developer_instructions = "Inspect the relevant sources and report evidence."
```

Other role-local config settings, such as `model` and
`model_reasoning_effort`, can be set in the same file. Relative config paths
resolve against that file's directory.

Hook-management authorization is inherited from the parent's effective
`hook_approval_policy`. Role files and built-in personas cannot override it;
selecting a role never opts a session into automatic hook authorization.

Roles are layered in increasing precedence: stored roles first, then each
trusted project `.chaos/agents/` folder from the project root toward the
working directory. A higher layer selects the role for a repeated name and
inherits omitted metadata from lower layers. A non-blank description is
required after this merge. Duplicate names within one folder, malformed files,
and malformed stored documents generate startup warnings and are skipped.
Disabled stored roles and untrusted project folders are not loaded.

## GLOBAL LIMITS

The `agents` user setting contains only operational limits:

```sh
chaos config set agents '{"max_threads": 4, "max_depth": 2, "job_max_runtime_seconds": 1800}'
```

Split `[agents.<role>]` declarations and their `config_file` indirection are no
longer supported and are rejected as unknown fields. Role metadata and
instructions belong together in standalone role files.

## SEE ALSO

[chaos-synopsis(7)](./chaos-synopsis.7.md),
[chaos-modes(7)](./chaos-modes.7.md)
