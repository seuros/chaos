# Database configuration and approvals

User settings and remembered approvals use the existing runtime database:
SQLite by default, or the configured PostgreSQL database/schema. Use one owner
per database/schema. Storage failure does not fall back to TOML or another
database.

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
- The owner-only `installation-id` is local application state, not an export.

Preference precedence remains system defaults, database user settings, trusted
project defaults, then temporary overrides. Administrator requirements constrain
the result independently.

Configured storage URLs retain precedence over `CHAOS_STORAGE_URL`.
`CHAOS_EGRESS_URL` retains its existing egress override behavior.
Bootstrap accepts connection references such as:

```toml
storage_url = "env:CHAOS_DATABASE_CONNECTION"
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
chaos config bootstrap set storage_url env:CHAOS_DATABASE_CONNECTION
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
