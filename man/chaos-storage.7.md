+++
title = "chaos-storage(7)"
summary = "Runtime storage, persisted history, and agent history access."
+++

# chaos-storage(7)

## NAME

chaos-storage - select and provision the FreeChaOS runtime storage backend

## DESCRIPTION

Select storage before using FreeChaOS. PostgreSQL supports shared multi-machine
history and semantic recall; SQLite is for one machine. Interactive startup
offers first-run setup; other commands require `storage_url` in advance.
Changing storage requires a restart. Settings, history, jobs, and global MCP
servers use the selected database.

Shell environment capture stays in memory. ChaOS no longer reads, creates, or
automatically removes the retired `$CHAOS_HOME/shell_snapshots` directory.
If an older installation left it behind, remove it manually when no older ChaOS
process is using it; snapshots may contain sensitive environment values.

## AGENT HISTORY ACCESS

When a compaction summary omits needed context, search your session history,
then page around relevant entries. History access does not automatically restore
the conversation. Textual tool arguments/results are available; hidden reasoning,
images, encrypted content, and telemetry are not.

## JOURNAL WRITER OWNERSHIP

Only one instance may write a session. If resume reports it is in use, close the
other instance and retry; after a crash, normally wait up to 30 seconds.
Do not delete ownership records to force resume.

Console exit gives all processes up to 10 seconds to shut down. Immediate exit
or a second termination signal skips the remaining grace period.

Journal lease failures pause work and retry automatically.
Interrupted chat turns continue from history after recovery;
an explicit interrupt while paused cancels that continuation. Standalone
shell commands, reviews, and compaction must be retried manually.
Ownership or history conflicts stay
paused: close the other writer and resume the session.

After explicit resume, automatic wakes remain interrupted until operator input.
Register machine-recovery waits anew; closed sessions do not recover unattended.

## SQLITE

The interactive console checks `storage_url` in `$CHAOS_HOME/config.toml`. If
it is missing or empty, onboarding asks you to choose PostgreSQL or SQLite
before loading database-backed settings. Selecting SQLite creates or opens
`chaos.sqlite` under the chaos home, applies its migrations, and saves the URL
in the bootstrap file.

A configured PostgreSQL/SQLite URL or reference such as `env:CHAOS_DATABASE_URL`
skips storage onboarding, and so does a non-empty `CHAOS_STORAGE_URL` in the
environment. Literal URLs need no environment variable. An existing SQLite file
on its own is not a choice.

First-run setup puts the SQLite file under the chaos home (`$CHAOS_HOME`, or
`~/.chaos`). To keep it elsewhere, name the file in the URL:
`storage_url = "sqlite:///other/disk/chaos.sqlite"`. The old `sqlite_home`
bootstrap key and the `CHAOS_SQLITE_HOME` variable are gone.

## POSTGRESQL

Point `storage_url` at a database and FreeChaOS mounts it instead:

```sh
export CHAOS_DATABASE_URL='postgres://chaos:PASSWORD@db.example.com:5432/chaos'
chaos config bootstrap set storage_url env:CHAOS_DATABASE_URL
```

The resulting `~/.chaos/config.toml` holds only that reference (and optionally
`egress_url`); every other setting lives in the database.

PostgreSQL 10 or later is required. Migrations run on first connect; an empty database is all
FreeChaOS needs — but it will not create the database or the role for you.
When PostgreSQL is selected, connection or migration failure stops startup.
FreeChaOS never falls back to SQLite for an explicitly selected PostgreSQL
backend.

Interactive onboarding saves credential-bearing URLs in the encrypted vault.
The OS credential store holds its unlock key. If that store is unavailable,
setup fails without writing plaintext; use an `env:VARIABLE` reference instead.
This also applies to FreeBSD hosts without D-Bus Secret Service.

### Shared credential vault

`$CHAOS_HOME/secrets/local.age` holds settings credentials, provider accounts in
`keyring`/`auto` mode, and named secrets. Its unlock key uses the OS credential
store. `auto` does not fall back to plaintext.

Legacy per-item Keychain items are never read or imported; credentials that
only live there must be re-entered. Back up both the encrypted vault and its
unlock key.

macOS may still authorize the initial unlock after restart or a binary change.
Locking Keychain does not revoke a key already held in process memory; restart
ChaOS to discard it.

### Provisioning

On the database host:

```sh
psql -U postgres <<'SQL'
CREATE ROLE chaos LOGIN PASSWORD 'PASSWORD';
SQL
psql -U postgres -c 'CREATE DATABASE chaos OWNER chaos'
```

`CREATE DATABASE` cannot run inside a transaction block, so it needs its own
invocation.

If the server is not on the same machine, it must accept the connection.
PostgreSQL listens on loopback only by default; add the interface to
`listen_addresses` in `postgresql.conf` and a matching line to `pg_hba.conf`:

```
host    chaos    chaos    192.168.0.0/24    scram-sha-256
```

`pg_hba.conf` alone is picked up by `SELECT pg_reload_conf()`. Changing
`listen_addresses` requires a restart.

### Semantic recall

`chaos-recall` stores embeddings in the same database and requires pgvector.
Install the extension package on the server; FreeChaOS issues
`CREATE EXTENSION IF NOT EXISTS vector` itself, which needs a role that may
create extensions. Recall is PostgreSQL-only and reports a backend error under
a SQLite mount.

## RESOLUTION ORDER

The backend is decided once, in this order:

1. `storage_url` in `~/.chaos/config.toml`
2. the `CHAOS_STORAGE_URL` environment variable

Nothing else is a choice: with neither, the console runs first-run
setup and every other command exits with instructions. The bootstrap file wins
over the environment, so a `storage_url` in place means `CHAOS_STORAGE_URL` is
ignored — including for a one-off shell. `-c storage_url=...` is rejected;
storage is never a session override.

Accepted schemes are `postgres://`, `postgresql://`, `sqlite:`, `sqlite://`,
and `sqlite3:` (rewritten to `sqlite:`). Anything else is rejected when the
config loads rather than at first query. `sqlite::memory:` gives a throwaway
backend that vanishes with the process:

```sh
CHAOS_STORAGE_URL=sqlite::memory: chaos exec "say hi"
```

## MIGRATING AN EXISTING INSTALL

There is no cross-backend export. A fresh database starts with empty history;
the old database remains untouched. To return to it, explicitly select its URL.
After upgrading, refresh provider catalogs online before relying on offline use.

## NOTES

Manually entered literal URLs sit in plaintext in the bootstrap file; keep it
at mode `0600`. Prefer onboarding's encrypted-vault reference or `env:VARIABLE`.
FreeChaOS does not read `.pgpass`.

## FILES

- `~/.chaos/config.toml` - bootstrap: `storage_url` and `egress_url` only
- `~/.chaos/chaos.sqlite` - the SQLite backend

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
