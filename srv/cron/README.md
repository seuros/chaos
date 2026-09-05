# chaos-cron

Scheduled task execution for the operator. Runs recurring jobs outside of active
model turns.

Scheduler and spool-executor construction are infallible. `spawn_scheduler`
returns the shutdown sender only to the caller that installs the process-wide
scheduler; concurrent or repeated callers receive `None`. Storage, command,
provider, network, and serialization failures are still reported while jobs
run.

## Local Postgres validation

The Postgres arms of this crate's suite skip themselves unless
`TEST_DATABASE_URL` names a reachable PostgreSQL 18 database. They share the
`cron_jobs` table with whatever else runs against it, so each one reads back
rows under a path it owns rather than the table as a whole.

```sh
just postgres-validate postgres://USER:PASSWORD@HOST:5432/DBNAME
```
