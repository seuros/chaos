# Model selection when resuming or forking

Interactive resume/fork (picker, ID/name, and `--last`), in-app `/resume` and
`/fork`, and `chaos exec resume` restore the last model and provider/account
used in a recorded turn, together with reasoning effort. Exec fork snapshots
use the same rules. Restoration does not change global configuration.

- A selection change followed by exit without another turn is not restored.
- An explicit `--model`, `--provider`, `-c model=...`, or
  `-c model_provider=...` bypasses saved-selection restoration and uses normal
  CLI/config resolution. Either flag overrides the selection as a whole.
- With no model/provider override, `-c model_reasoning_effort=...` overrides
  only the saved effort.
- In the picker, Tab keeps the current model/provider/effort instead.
  This works even when the saved provider matches the current one.
- `--last` finds the most recently updated eligible conversation across
  providers, respecting the current directory unless `--all` is supplied.

A missing or unusable saved provider is not silently replaced. Configure or
authenticate it, explicitly choose a model/provider, or use Tab in the picker.
In-app failures leave the current conversation open.

Turn records require the provider/account ID alongside the model. There is no
runtime legacy-format fallback. Credentials are never saved in turn records.
If the retained history has no used turn (for example, after rolling back every
turn), Chaos warns and keeps the current selection. Rollback and fork history
cutoffs exclude discarded turns.

## Upgrading to 47.4.0

1. Stop older Chaos clients and journal writers, including any running journald.
2. Back up the configured SQLite or PostgreSQL database.
3. Deploy the upgraded harness and restart it. The normal database-open path
   applies migration 0016 once, before journals are decoded. For SQLite, restart
   journald with the upgraded binary too.
4. Resume the conversation normally.

The migration fills only absent/null turn providers, using the nearest preceding
session-metadata provider, or the recorded process provider when no such metadata
exists. Existing turn providers, models, effort, sequence numbers, timestamps and
session ordering are unchanged. Append-only protections remain enabled after the
transaction; a failed migration rolls back the repair and guard changes together.

Provider switches that were never recorded cannot be reconstructed. If the
recorded provider is unknown, unavailable, or incorrect for that turn, explicitly
choose the intended provider/model when resuming. The migration never substitutes
the current global default. Do not restart older writers after upgrading.

PostgreSQL migration lock acquisition times out after five seconds rather than
waiting indefinitely on active writers; stop them and retry startup if that occurs.
