# Getting started

FreeChaOS keeps everything it remembers in one database: your settings,
providers, approvals, history, and MCP registrations. The only file it needs
before that database exists is a two-line bootstrap file that says where the
database is. This guide gets you from an installed binary to a working session.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/seuros/chaos/master/install.sh | sh
```

See [chaos-install(7)](../man/chaos-install.7.md) for building from source,
NixOS, and system requirements.

## 2. Choose where the database lives

Pick one backend. There is no silent default: every `chaos` command refuses to
run until storage is configured.

### PostgreSQL (recommended)

PostgreSQL lets several machines share one history, enables semantic recall,
and keeps your state off the laptop disk. FreeChaOS creates its own tables but
does not create the database or role. On the database host:

```sh
psql -U postgres <<'SQL'
CREATE ROLE chaos LOGIN PASSWORD 'change-me';
SQL
psql -U postgres -c 'CREATE DATABASE chaos OWNER chaos'
```

No server yet? A local container is enough to start (podman runs on Linux
and FreeBSD):

```sh
podman run -d --name chaos-pg -e POSTGRES_USER=chaos -e POSTGRES_PASSWORD=change-me \
  -e POSTGRES_DB=chaos -p 5432:5432 docker.io/pgvector/pgvector:pg18
```

The `pgvector` image also provides the extension that semantic recall uses.

Keep the credential out of the bootstrap file by exporting it:

```sh
export CHAOS_DATABASE_URL='postgresql://chaos:change-me@localhost:5432/chaos'
```

### SQLite

One machine, no server. FreeChaOS creates `~/.chaos/chaos.sqlite` on first run.
Semantic recall is unavailable on SQLite.

## 3. First run

### Interactive

Run `chaos`. The console asks which backend to use before loading anything else.

- Choose **PostgreSQL** and enter `env:CHAOS_DATABASE_URL` (or a literal URL,
  which is stored in the encrypted vault, never in plaintext).
- Choose **SQLite** to use the file under `~/.chaos`.

Setup connects, initializes the tables, and only then writes the bootstrap file.
A failed connection stays on the setup screen so you can fix the URL.

### Headless servers and CI

Write the bootstrap without starting the console:

```sh
chaos config bootstrap set storage_url env:CHAOS_DATABASE_URL   # PostgreSQL
chaos config bootstrap set storage_url sqlite:///var/lib/chaos/chaos.sqlite
```

Or export `CHAOS_STORAGE_URL` for the process; the bootstrap file wins when
both are present. Tables are created on the first command that opens storage.

## 4. Add a provider

Providers are user settings in the database. Values are JSON:

```sh
chaos config set model_providers.local \
  '{"name":"Local server","base_url":"http://127.0.0.1:8000/v1","wire_api":"chat_completions"}'
chaos config set model_provider '"local"'
chaos config set model '"<model id>"'
```

Hosted providers that need a key use `env_key`; see
[chaos-providers(7)](../man/chaos-providers.7.md) and
[chaos-local-models(7)](../man/chaos-local-models.7.md).

## 5. Optional: route providers through a gateway

`egress_url` is the second and last bootstrap key:

```sh
chaos config bootstrap set egress_url https://gateway.example/egress/chaos
```

`CHAOS_EGRESS_URL` overrides it for a single run.

## Where things live

| What | Where | How to change |
|---|---|---|
| `storage_url`, `egress_url` | `~/.chaos/config.toml` | `chaos config bootstrap set` |
| Every other setting | the runtime database | `chaos config get/set/unset` |
| Settings backup | JSON | `chaos config export > settings.json` / `chaos config import settings.json` |
| Approvals | the runtime database | `chaos approvals list/revoke` |
| Project MCP servers | `<project>/.mcp.json` | edit the file |
| Admin constraints | `/etc/chaos/requirements.toml` | edit the file |
| Agent roles | the runtime database | `chaos agents import/list/remove` |
| Your exec-policy rules (restrict only) | the runtime database | `chaos rules import/list/remove` |
| Admin rules (may grant) | `/etc/chaos/rules/*.decrees` | edit the files |

The bootstrap file accepts only those two keys. Anything else in it stops
startup with a message naming the key and the `chaos config set` command that
replaces it. Check your setup at any time with `chaos config doctor`.

## Next

- [chaos-storage(7)](../man/chaos-storage.7.md): backends, provisioning, recall.
- [chaos-mcp(7)](../man/chaos-mcp.7.md): connecting tools.
- [Database configuration and approvals](./database-configuration.md): the
  trust and approval model behind the database.
