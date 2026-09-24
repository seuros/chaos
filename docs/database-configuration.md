# Database configuration and approvals

User settings and remembered approvals use the existing runtime database:
SQLite by default, or the configured PostgreSQL database/schema. Use one owner
per database/schema. Storage failure does not fall back to TOML or another
database.

## First-run storage setup

When `storage_url` is missing or empty in `$CHAOS_HOME/config.toml`, the interactive
console asks where to store data before loading database-backed settings:

- **PostgreSQL (Recommended)** connects to an existing database on a local or
  remote PostgreSQL server. Enter a `postgresql://user:password@host:5432/database`
  URL or an `env:VARIABLE` reference. ChaOS initializes its tables; it does not
  install PostgreSQL or create the database itself. Use one owner per
  database/schema.
- **SQLite** creates `chaos.sqlite` in the ChaOS home, with no server required.

Connection input is shown as entered. Literal PostgreSQL URLs use the secure
credential store when available; environment references are saved unchanged.
On FreeBSD, D-Bus Secret Service is optional and often absent in headless sessions.
If the secure store is unavailable, onboarding saves the URL directly in the
existing `$CHAOS_HOME/config.toml`, atomically replacing it with an owner-only
file (`0600`). The screen discloses this before submission: the URL is **not
encrypted** and is accessible to your account and root. No separate credential
file or environment setup is required for subsequent launches.

Use `env:VARIABLE` if you do not want the URL saved in TOML. Other platforms still
require an environment reference when the secure store is unavailable. This
fallback applies only to onboarding bootstrap connections, not other settings
credentials or existing keyring references. Treat config backups as sensitive.

Setup saves `storage_url` only after successful database initialization.
Connection failures stay on the setup screen for retry or a different choice;
there is no automatic SQLite fallback. Escape goes back or quits, and Ctrl+C
quits without saving a choice.

A non-empty `storage_url` in `config.toml` bypasses this screen: use a PostgreSQL or
SQLite URL, or a reference such as `env:CHAOS_DATABASE_URL`. Literal URLs do not
require an environment variable. Configured references are resolved during normal
startup; a missing referenced variable or unavailable keyring remains an error,
not a reason to silently select another database.

Environment variables alone (`CHAOS_STORAGE_URL`, `CHAOS_DATABASE_URL`, or
`CHAOS_SQLITE_HOME`), an existing local database, and `installation-id` do not
replace the TOML storage choice. Non-interactive commands retain their existing
environment/default SQLite behavior.

## Ownership

- `~/.chaos/config.toml` contains bootstrap `storage_url` and `egress_url`.
  Legacy `sqlite_home` is accepted during migration.
- Preferences, profiles, providers, notices, trust decisions, MCP registrations,
  and remembered approvals are database-backed.
- Project defaults, instructions, `.mcp.json`, standalone `agents/` documents,
  and administrator policy remain files.
- Project configuration cannot redirect bootstrap or grant security authority.
  Project rules can restrict authority, not supply positive grants.
- Migrated literal credentials use opaque keyring references. macOS uses the
  native login Keychain. Existing login/OAuth storage options retain their own
  behavior; this migration adds no plaintext fallback for settings credentials.
  The disclosed FreeBSD onboarding bootstrap fallback is separate.
- The owner-only `installation-id` is local application state, not an export.

Preference precedence remains system defaults, database user settings, trusted
project defaults, then temporary overrides. Administrator requirements constrain
the result independently.

Configured storage URLs retain precedence over `CHAOS_STORAGE_URL`.
`CHAOS_EGRESS_URL` retains its existing egress override behavior.
Bootstrap accepts connection references such as:

```toml
storage_url = "env:CHAOS_DATABASE_URL"
egress_url = "https://gateway.example/egress/chaos"
```

Changing storage destinations requires restart. Recovery commands validate
literal URLs using existing storage/egress validators and share the migration
lock. They do not resolve credential references merely to save an offline edit.

## Migration

Stop older ChaOS processes before migrating. Do not run older binaries against
the migrated installation.

```sh
chaos config migrate --dry-run
chaos config migrate
chaos config doctor
```

Dry-run does not write settings or access the keychain. Actual migration
validates settings, creates an owner-only TOML backup, and imports preferences,
restrictive user rules, and inactive historical grants. A source digest makes
interrupted TOML cleanup retryable without overwriting newer database edits.

Old positive MCP, connector, shell, and network grants require reapproval.
After migration, legacy user `rules/*.decrees` files are no longer live policy.
Executable identity mappings must be defined in administrator policy.

Explicit credential fields and environment/header values in global MCP
registrations move to the keyring before references are persisted. Endpoint
userinfo must instead be moved to bearer-token/header references. Arbitrary
secrets in commands, arguments, or free text cannot be identified automatically.
Backups and historical database pages may retain old credentials; rotate them
when necessary.

## Commands

```sh
chaos config get
chaos config get model
chaos config set model '"your-model-id"'
chaos config unset model
chaos config export > settings.toml
chaos config import settings.toml
chaos approvals list
chaos approvals revoke <grant-id>
chaos approvals revoke all
```

`set` accepts JSON. Import replaces the user settings document with validated
TOML using optimistic revision checking. Neither import nor ordinary edits can
create positive approval grants. Export excludes live grants and secret values;
it does not include the separately managed MCP registry or project/admin files.

Recovery without starting providers, MCP servers, or schedulers:

```sh
chaos config bootstrap show
chaos config bootstrap set storage_url env:CHAOS_DATABASE_URL
```

Inspection redacts bootstrap connection values and preserves opaque credential
references. API user layers use `userDatabase` with a revision; writes return
`filePath: null` and an explicit source. Stale revisions are rejected.

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
session after CLI preference edits. Automatic preference refresh and exhaustive
per-field source-classification coverage remain follow-up work.

For restoration, stop sessions, restore storage, and revoke all installation-local
grants before restarting. A different installation must retain its own new
installation identity. Hostile rollback of both database and local state cannot
be detected reliably without an external monotonic authority. Audit events are
operational history, not tamper-proof evidence against the database owner.
