# Database configuration and approvals

User settings and remembered approvals live in the runtime database: SQLite or
the configured PostgreSQL database/schema. Use one owner per database/schema.
Storage failure never falls back to a file or another database.

## Bootstrap

`$CHAOS_HOME/config.toml` holds exactly two keys and nothing else:

```toml
storage_url = "env:CHAOS_DATABASE_URL"
egress_url = "https://gateway.example/egress/chaos"
```

Both accept a literal URL, an `env:VARIABLE` reference, or a credential
reference into the local vault. Any other key, including the retired
`sqlite_home`, stops every command with a message naming the key and the
`chaos config set` command that replaces it. The file is not a configuration
layer; it is read once to find the database.

Storage is never chosen silently. Resolution order:

1. `storage_url` in the bootstrap file
2. `CHAOS_STORAGE_URL`

When neither applies, interactive `chaos` runs first-run storage setup; every other
command exits with instructions. `CHAOS_EGRESS_URL` overrides `egress_url`.
A running process keeps the storage it resolved at startup if the bootstrap
file is removed; a file that is present but invalid stops the next storage
access.

## First-run storage setup

When the bootstrap file has no `storage_url` and `CHAOS_STORAGE_URL` is unset,
the console asks where to store data before loading database-backed settings:

- **PostgreSQL (Recommended)** connects to an existing database on a local or
  remote PostgreSQL server. Enter a `postgresql://user:password@host:5432/database`
  URL or an `env:VARIABLE` reference. ChaOS initializes its tables; it does not
  install PostgreSQL or create the database itself.
- **SQLite** creates `chaos.sqlite` in the ChaOS home, with no server required.

Literal PostgreSQL URLs go to the encrypted vault; environment references are
saved unchanged. The OS credential store holds only the vault's unlock key. If
it is unavailable, onboarding fails without writing plaintext. Use
`env:VARIABLE` when no OS credential store is available (including headless
FreeBSD).

Setup saves `storage_url` only after successful database initialization.
Connection failures stay on the setup screen for retry or a different choice.
Escape goes back or quits, and Ctrl+C quits without saving a choice.

Headless installs skip the screen with
`chaos config bootstrap set storage_url <url|env:VAR>` or `CHAOS_STORAGE_URL`.
See [Getting started](./getting-started.md).

## Ownership

- Preferences, profiles, providers, notices, trust decisions, MCP registrations,
  and remembered approvals are database-backed.
- Global and project lifecycle hooks are database-backed, managed with
  `chaos hooks` or policy-authorized `hooks_*` tools (human-approved by default).
  `hooks.json` files are never loaded at runtime. Project hooks also require
  project trust. See [chaos-hooks(7)](../man/chaos-hooks.7.md).
- Agent roles are database-backed, managed with `chaos agents`. A trusted
  project may still ship roles as files under `.chaos/agents/`; those overlay
  the stored ones.
- Custom modes, prompts, syntax themes, and halluacinate scripts are
  database-backed, managed with `chaos modes`, `chaos prompts`,
  `chaos themes`, and `chaos scripts`. `$CHAOS_HOME/modes/`,
  `$CHAOS_HOME/prompts/`, `$CHAOS_HOME/themes/`, and
  `~/.config/chaos/scripts/` are no longer read. A trusted project may still
  ship `.chaos/scripts/`; untrusted projects' scripts never run.
- Project `.mcp.json` and project instructions remain files. There is no
  project `config.toml` and no system `config.toml`.
- Administrator constraints live in `/etc/chaos/requirements.toml`, including
  network policy under `[experimental_network]`. Administrator rules in
  `/etc/chaos/rules/*.decrees` are the only rules that can grant.
- Your own exec-policy rules are database-backed, managed with `chaos rules`
  (`import <file.decrees>`, `list`, `show`, `remove`). They can only restrict:
  a document with an `allow` decision is refused at import. A trusted
  project's `.chaos/rules/` files also restrict only. `$CHAOS_HOME/rules/` is
  no longer read.
- Credentials use opaque references backed by `$CHAOS_HOME/secrets/local.age`.
  macOS keeps the vault's single unlock key in the native login Keychain.
  Provider auth modes `keyring` and `auto` use this same vault with no plaintext
  fallback. Provider `file` (the existing default) and `ephemeral` modes remain
  unchanged.
- The owner-only `installation-id` is local application state, not an export.

Preference precedence is system defaults, database user settings, then
temporary session overrides. Administrator requirements constrain the result
independently. Project trust gates `.mcp.json`, project rules, and project
agent roles; it never supplies positive grants.

Changing storage destinations requires a restart. Recovery commands validate
literal URLs with the existing storage/egress validators and never resolve
credential references merely to save an offline edit.

## Upgrading from TOML-era installs

47.14.0 removed `chaos config migrate` and every TOML configuration layer. If a
pre-47.14 bootstrap file still carries settings, startup names the first
offending key; move it with `chaos config set <key> <json>` and delete it from
the file. A `sqlite_home` key must be replaced with an explicit
`sqlite:///path/chaos.sqlite` storage URL; `CHAOS_SQLITE_HOME` is gone too. `/etc/chaos/config.toml` and
`.chaos/config.toml` files are no longer read; admin network tables move to
`requirements.toml`. Settings already in the database are unaffected.

## Credential vault and macOS prompts

Settings, bootstrap connection references, provider accounts in `keyring`/`auto`
mode, and named secrets share one encrypted vault per ChaOS home. Its unlock key
is loaded once per process and kept in memory, never written to plaintext disk.
Concurrent callers share that lookup. The decrypted snapshot is reused only
while the ciphertext is unchanged; writes and deletions by other processes are
observed on subsequent vault reads. Mutations are locked across processes and
persisted atomically with owner-only permissions. Missing keys or corrupt
ciphertext fail closed; an existing vault never gets a replacement unlock key.

The `keyring:chaos-settings/UUID` reference syntax addresses records inside this
home-local vault. References alone are not portable between homes. Back up the
encrypted vault **and** its OS-held key. Vault schema v2 is not writable by
pre-47.8 ChaOS versions.

macOS can still ask for authorization after restart or binary-signature changes.
Locking Keychain does not erase a running process's already unlocked key;
restart ChaOS to discard it. External master-key changes likewise require a
restart.

## Commands

```sh
chaos config get
chaos config get model
chaos config set model '"your-model-id"'
chaos config unset model
chaos config export > settings.json
chaos config import settings.json
chaos approvals list
chaos approvals revoke <grant-id>
chaos approvals revoke all
```

`set` accepts JSON. Import replaces the user settings document with a validated
JSON document using optimistic revision checking. Neither import nor ordinary
edits can create positive approval grants. Export excludes live grants and
secret values; it does not include the separately managed MCP registry or
project/admin files.

Recovery without starting providers, MCP servers, or schedulers:

```sh
chaos config bootstrap show
chaos config bootstrap set storage_url env:CHAOS_DATABASE_URL
chaos config doctor
```

Inspection redacts bootstrap connection values and preserves opaque credential
references. API user layers use `userDatabase` with a revision; writes return an
explicit source. Stale revisions are rejected.

## Approval boundaries and current limits

Global MCP grants are installation-local and bound to effective registration
and tool identity. Project overrides additionally bind to canonical workspace
scope. Shell/network grants bind to installation and canonical workspace; outside
a Git repository, scope is the canonical working directory.

MCP identity includes transport, resolved executable/endpoint, arguments,
working directory, explicit environment/header and credential references,
connector, input schema, and security annotations. It excludes display text,
timeouts, map ordering, and raw secret values. This is configuration identity,
not binary or remote-server attestation.

Approval lookups read current database state. Revocation invalidates session
approval caches through a revision counter; it does not cancel already-authorized
work. Failed persistent writes are denied, not silently converted to session
approval. Retry with an explicit one-shot decision if appropriate.

New turns require readable authoritative settings. Ordinary preferences remain
session snapshots unless an existing UI action refreshes them; start a new
session after CLI preference edits.

For restoration, stop sessions, restore storage, and revoke all installation-local
grants before restarting. A different installation must retain its own new
installation identity. Hostile rollback of both database and local state cannot
be detected reliably without an external monotonic authority. Audit events are
operational history, not tamper-proof evidence against the database owner.
