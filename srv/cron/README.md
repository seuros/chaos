# chaos-cron

Scheduled task execution for the operator. Runs recurring jobs outside of active
model turns.

## Local Postgres validation

The Postgres coverage here is intentionally bounded to the new storage path:

```sh
cargo test -p chaos-cron postgres_ -- --nocapture
```

Set `TEST_DATABASE_URL` to a local PostgreSQL 18 database, or use:

```sh
just postgres-validate-cron postgres://USER:PASSWORD@HOST:5432/DBNAME
```
