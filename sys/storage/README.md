# chaos-storage

Backend-agnostic persistence layer. SQLite for the common case; Postgres when the
operator needs it. Everything that needs to survive a restart goes through here.

## In-memory SQLite validation

For test isolation, set `CHAOS_STORAGE_URL` before starting the test process:

```sh
CHAOS_STORAGE_URL=sqlite::memory: cargo test -p libui
```

Set this outside the test process rather than mutating environment variables from
individual parallel tests.

## Local Postgres validation

The bounded Postgres validation path is env-gated so the normal test suite stays
cheap:

```sh
cargo test -p chaos-storage postgres_ -- --nocapture
```

Point it at a local PostgreSQL 18 database by exporting `TEST_DATABASE_URL`,
or use:

```sh
just postgres-validate-storage postgres://USER:PASSWORD@HOST:5432/DBNAME
```
