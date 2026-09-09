# chaos-agents(7)

## NAME

chaos-agents - standalone agent role definitions

## ROLE FILES

Define user roles in `.toml` files or Markdown files with TOML frontmatter
under `$CHAOS_HOME/agents/` (normally `~/.chaos/agents/`) or a trusted project's
`.chaos/agents/`. Subdirectories are scanned recursively.

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

Higher-precedence config layers select the role file for a repeated name and
inherit omitted metadata from lower layers. A non-blank description is required
after this merge. Duplicate names within one layer and malformed files generate
startup warnings; malformed definitions are skipped. Untrusted project layers
are not loaded.

## GLOBAL LIMITS

The `[agents]` table in `config.toml` contains only operational limits:

```toml
[agents]
max_threads = 4
max_depth = 2
job_max_runtime_seconds = 1800
```

Split `[agents.<role>]` declarations and their `config_file` indirection are no
longer supported and are rejected as unknown fields. Role metadata and
instructions belong together in standalone role files.

## SEE ALSO

[chaos-synopsis(7)](./chaos-synopsis.7.md),
[chaos-modes(7)](./chaos-modes.7.md)
