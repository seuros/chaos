# chaos-minions

Minions are bounded workers entrusted with delegated tasks. The parent
session keeps responsibility for integrating their work. Minions need only
the context required for their task; they need not inherit the parent's identity.

The crate currently owns the canonical minion nickname list. Kernel spawn,
instruction placement, and CSV fanout live in `chaos-kern`.

## Why minions

Minions are sloppy subagents. They take instructions from the coordinator, not
from the user. If the user did not express themselves correctly, the coordinator
guesses and hands the minions work that is not aligned. The name is the check:
a coordinator that sees *minions* double-checks before it delegates.

Calling them juniors is another way to see it, but juniors get experience and stop being
junior. Minions stay minions forever, no matter how frontier the model is.

Agents with a high level of intelligence can cause chaos. They can open pull
requests, comment on issues, close issues. They do this because they see a
better-articulated user — which is another agent — giving them instructions.

## Instruction scope

Use two distinct settings in `config.toml`:

```toml
# Applied to this session as a developer message.
developer_instructions = "Explain consequential actions before taking them."

# Kept as configuration in the parent, but not sent to the parent's model.
# Applied to delegated minions in addition to their selected role.
minion_instructions = """
You are a minion working on a bounded task.
Keep authorship clear and preserve work you did not create.
"""
```

`minion_instructions` is optional. It is included only in the initial developer
context of delegated process-spawn sessions, including nested minions. The same
scope check applies when initial context is rebuilt. CLI, API, MCP and other main
sessions do not receive it. Internal reviewers, compactors and memory workers
keep their own instruction contracts instead.

Agent-role Markdown bodies and TOML `developer_instructions` select the minion's
role; they do not replace project `minion_instructions`. Reviewer personas and
collaboration modes also use `developer_instructions`, independently of the
minion-only setting. A role configuration can explicitly override
`minion_instructions` through normal configuration precedence.

These settings control instruction placement, not context size. Use
`fork_context = false` (the default) for a lightweight minion and provide bounded
task context. Explicit forks intentionally carry conversation history; this
setting does not redact that history or remove other inherited configuration.

CSV delegation tools are `spawn_minions_on_csv` and `report_minion_job_result`.
Job rows stay in `agent_jobs` and `agent_job_items`. Editing configuration does
not retract a developer message already present in an existing session's history.
