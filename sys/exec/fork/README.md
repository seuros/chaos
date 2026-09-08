# chaos-fork

The init process. Parses CLI args, wires up auth and config,
then fork+execs the agent in headless or JSONL mode.

Resume and fork snapshots restore the last-used model/provider and reasoning
effort unless explicitly overridden. See
[resume model selection](../../../docs/resume-model-selection.md) for precedence
and failure behavior.
