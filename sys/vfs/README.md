# chaos-vfs

The virtual filesystem over the two persistence backends. SQLite for the common
case, PostgreSQL when the operator asks for it. Everything that must survive a
restart goes through here.

One backend is resolved at boot, mounted, and served to every consumer for the
life of the process. Consumers read `chaos_vfs::root()` or `chaos_vfs::pool()`
and match on `Vfs`; they do not resolve a backend of their own.
`backend_dispatch!` writes the two-arm match for traits whose implementations
differ per backend.

Operator-facing setup — provisioning, `storage_url`, resolution order — is
[chaos-storage(7)](../../man/chaos-storage.7.md).

## In-memory SQLite validation

For test isolation, set `CHAOS_STORAGE_URL` before starting the test process:

```sh
CHAOS_STORAGE_URL=sqlite::memory: cargo test -p libui
```

Set this outside the test process rather than mutating environment variables from
individual parallel tests.

## Local Postgres validation

The Postgres arms of the suite skip themselves unless `TEST_DATABASE_URL` names a
reachable database. Point them at one with:

```sh
just postgres-validate postgres://USER:PASSWORD@HOST:5432/DBNAME
```

That covers this crate along with `chaos-cron`, `chaos-proc`, and `chaos-recall`.
Adding a migration under `var/proc/db/migrate/` does not invalidate the embedded
migrator's build cache, so touch `var/proc/src/migrations.rs` before running, or
the suite fails on a migration it has already applied.
