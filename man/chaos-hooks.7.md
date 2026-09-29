+++
title = "chaos-hooks(7)"
summary = "Database-backed lifecycle hooks, authorization, and execution."
+++

# chaos-hooks(7)

## NAME

chaos-hooks - manage database-backed lifecycle hooks

## DESCRIPTION

Hooks run shell commands on `session_start`, `before_turn`, or `stop`. Definitions
live in the existing SQLite/PostgreSQL database, independently of user settings.
Neither global, system, nor project `hooks.json` files are loaded at runtime.

## MANAGEMENT

```sh
chaos hooks list
chaos hooks add context --event before-turn --command 'git status --short'
chaos hooks show context
chaos hooks enable context
chaos hooks disable context
chaos hooks update context '{"event":"before_turn","command":"git diff --stat"}'
chaos hooks remove context
```

Add `--project` to register for the current canonical project root. Otherwise the
hook is global. IDs are unique in the database. Add defaults to disabled;
`--enabled` explicitly requests recurring execution. Update replaces the whole
definition and preserves enabled state. CLI mutations require interactive confirmation unless the operator explicitly
supplies `--yes` (see HEADLESS PROVISIONING); inspection never executes commands.

`chaos hooks import /path/to/hooks.json --prefix legacy [--project]` is an
explicit one-time import. It validates the entire document, rejects unsupported
handlers, and imports atomically as disabled hooks. Existing IDs cause rollback,
so retries cannot duplicate hooks. Original files are not deleted. Enable each
imported hook separately.

## MODEL ACCESS

- `chaos://hooks`: visible global/current-project definitions, revisions,
  installation approval state, and reasons a hook is inactive.
- `chaos://hooks/{id}`: one visible definition.
- `hooks_create`, `hooks_update`, `hooks_set_enabled`, `hooks_delete`: manage a
  change under `hook_approval_policy`. The default `on-request` policy waits for
  human form elicitation, including disable/delete.
- `hooks_preview`: validate a proposed action without writing or executing.

Read resources again after changes; built-in subscriptions return snapshots.
Management tools are in the `session` capability group and mutations obey the
active mode's mutation capability.

Under `on-request`, the human sees the exact old/new definition, event, project scope, current
working directory, timeout, and whether recurring execution is being authorized.
The client must return `accept` with `approve: true`. Decline, cancel, timeout,
disconnect, headless mode, and unavailable elicitation never change a hook.
There is no model-callable approval endpoint or `approved` input. Updates require
the current `expected_revision`; a concurrent change invalidates the proposal.
Creating with the same ID never creates a duplicate.

Approvals are installation-local and bound to the exact stored revision. Editing
a hook invalidates previous approvals on all installations; only the approving
installation receives the new grant. Generic config import does not activate
hooks. `chaos approvals revoke` also revokes hook authorization.

## HEADLESS PROVISIONING

An operator can authorize one exact CLI mutation without a TTY:

```sh
chaos hooks --yes add house-context --event before-turn \
  --command 'python3 /home/agent/identity/automation/memory_before_turn.py' --enabled
chaos hooks --yes import /path/to/hooks.json --prefix legacy
chaos hooks --yes enable legacy-1
```

`--yes` works for add, update, enable, disable, remove, and import. It does not
execute a hook or change the policy for subsequent commands. Import still creates
**disabled** hooks: inspect `chaos hooks list` and explicitly enable each desired
ID. No runtime file discovery is restored. The command refuses agent shells
(`CHAOS_THREAD_ID` is present), even with `--yes`; agents must use the native hook
tools. Do not unset that marker to bypass the policy.

For unattended resident-authored hooks, the operator can separately opt into
standing authorization before launching the resident:

```sh
chaos config set hook_approval_policy '"automatic"'
```

`automatic` permits hook-tool create/update/enable/disable/delete without human
elicitation, including headless sessions. `on-request` is the default; headless
execution alone does **not** opt in. `hooks_preview` remains read-only. Policy
uses the effective session configuration; changing stored settings requires a
new session to reliably take effect. Project configuration cannot set this
authority-bearing policy. The tools do not accept approval fields.

Both routes use the same validated, revision-checked writes and installation-local
grants as interactive approval. Project scope/trust, active-mode mutation rules,
execution-policy denies, sandboxing, and environment filtering still apply.
Automatic management does not resurrect revoked approvals during execution.
An explicit enable authorizes the current revision on this installation again.
Switching back to `on-request` governs future changes; use `chaos approvals revoke`
separately to revoke existing execution grants.

This grants management of all visible hooks, including operator-installed hooks.
It is not a managed-versus-resident ownership boundary. Treat hook configuration
and database access as trusted operator/resident authority; OS permissions and
separate homes/databases must enforce any stronger separation. A resident with
arbitrary shell/database access is not contained by management-tool policy alone.

### Hosted rollout and repeated boot

Persist both the database and installation identity with the resident's Chaos
home. Provision before accepting turns, under the resident's OS identity and
against its actual `CHAOS_HOME`/storage destination. Reopening the same home keeps
grants; a different installation needs its own explicit authorization.

Use stable IDs and inspect before reconciling. Blindly repeating an import fails
atomically on existing IDs; it never silently duplicates or overwrites them.
Do not unconditionally rewrite definitions at every boot: an update creates a
new revision and invalidates the old grant on other installations. Preserve
resident modifications and disabled/revoked state unless the operator deliberately
chooses to change them. Snapshot the old database/home before migration; rolling
back the binary alone does not roll back its state.

## EXECUTION

Project hooks require current database project trust. The kernel reloads hooks
at lifecycle boundaries; preview and execution share a snapshot. Changes do not
interrupt a dispatch already running. Storage/validation failures stop the turn
rather than silently skipping hooks. No filesystem fallback exists.

Commands run in the session's working directory with its filtered environment,
current filesystem/network sandbox, and execution-policy deny checks. Hook
approval does not grant sandbox escalation. Timeout covers stdin, process exit,
and output draining (default 30 seconds, maximum 600); each output stream is
limited to 128 KiB. Hook process groups are terminated on cancellation/timeout.
Existing event input/output JSON semantics are unchanged. Results are ordered
global-before-project, then by `order` and stable ID; matching commands can run
concurrently. Event origins use `chaos://hooks/{id}`.

Do not embed credentials in command strings; those strings are visible through
resources and approvals. Refer to environment variables instead. Approval gates
protect managed APIs, not an attacker with root access or database credentials.

## SEE ALSO

- [chaos-mcp.7](./chaos-mcp.7.md)
- [chaos-storage.7](./chaos-storage.7.md)
