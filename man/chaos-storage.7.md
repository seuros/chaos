# chaos-storage(7)

## NAME

chaos-storage - select and provision the FreeChaOS runtime storage backend

## DESCRIPTION

Everything FreeChaOS must remember across a restart lives in one database:
processes and their lineage, the append-only journal, message history, cron
jobs and the spool, token accounting, the model catalog cache, project trust
decisions, and globally registered MCP servers.

That database is mounted once at boot and never swapped for the life of the
process. Two backends exist. SQLite is the default and needs no configuration.
PostgreSQL is opt-in, for operators who want several machines reading the same
history, or who want the semantic recall store.

## AGENT HISTORY ACCESS

Agents can inspect a bounded view of their own canonical persisted transcript
with two read-only tools:

- `read_session_history` pages backward through transcript entries. With no
  cursor it begins immediately before the latest compaction, or at the journal
  end when the session has not compacted.
- `search_session_history` performs bounded literal search across the current
  session's canonical journal. ASCII matching is case-insensitive; other text
  is matched exactly.

Both tools flush already-queued journal writes before reading, preserve journal
sequence numbers and timestamps, and return pagination cursors. They are
restricted to the calling session and omit hidden reasoning, encrypted
payloads, image data, turn-context records, and telemetry. Tool arguments and
textual tool results remain visible because they are often necessary to recover
operational context.

This access does not automatically restore old history into the active context
or require an agent to preserve anything. It gives the continuing agent a
deliberate route back when a compaction summary proves incomplete.

## SQLITE

The default. On first run FreeChaOS creates `chaos.sqlite` under the chaos
home and applies its migrations. Nothing to configure.

The home is the `sqlite_home` config key when set, then `$CHAOS_SQLITE_HOME`,
then the chaos home — `$CHAOS_HOME`, or `~/.chaos`. A relative
`$CHAOS_SQLITE_HOME` is taken against the working directory.

## POSTGRESQL

Point `storage_url` at a database and FreeChaOS mounts it instead:

```toml
# ~/.chaos/config.toml
storage_url = "postgres://chaos:PASSWORD@db.example.com:5432/chaos"
```

The schema is tested against PostgreSQL 18 and uses nothing newer than
PostgreSQL 10. Migrations run on first connect, so an empty database is all
FreeChaOS needs — but it will not create the database or the role for you.
When PostgreSQL is selected, connection or migration failure stops startup.
FreeChaOS never falls back to SQLite for an explicitly selected PostgreSQL
backend.

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
3. `chaos.sqlite` under the chaos home

Config wins over the environment, so a `storage_url` in place means
`CHAOS_STORAGE_URL` is ignored — including for a one-off shell.

Accepted schemes are `postgres://`, `postgresql://`, `sqlite:`, `sqlite://`,
and `sqlite3:` (rewritten to `sqlite:`). Anything else is rejected when the
config loads rather than at first query. `sqlite::memory:` gives a throwaway
backend that vanishes with the process, which is what test runs want:

```sh
CHAOS_STORAGE_URL=sqlite::memory: cargo test -p libui
```

Because config wins, that only takes effect when `storage_url` is unset. With
one in place, a one-off backend means passing it as a config override instead:

```sh
chaos -c storage_url=sqlite::memory: exec "say hi"
```

## MIGRATING AN EXISTING INSTALL

There is no export path between the two backends. Pointing `storage_url` at a
fresh PostgreSQL database gives you an empty history; the old
`~/.chaos/chaos.sqlite` stays where it is and is picked up again the moment
`storage_url` is removed.

## NOTES

The credential sits in plain text in `config.toml`. Keep the file at mode
`0600`; FreeChaOS does not read `.pgpass` or a keyring.

`chaos_journald` is only started for a mounted SQLite backend. PostgreSQL uses
the mounted pool directly and does not create or write `chaos.sqlite`.

## FILES

- `~/.chaos/config.toml` - where `storage_url` goes
- `~/.chaos/chaos.sqlite` - the default SQLite backend

## SEE ALSO

- [chaos-install.7](./chaos-install.7.md)
- [chaos-providers.7](./chaos-providers.7.md)
- [chaos-mcp.7](./chaos-mcp.7.md)
