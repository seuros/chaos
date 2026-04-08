# chaos-proc

SQLite-backed /proc for agent state. Indexes rollout metadata,
thread history, backfill progress, job claims, and tracing logs into a single local runtime DB.

This crate is also the backend home for log tailing/query APIs used by
dmesg-style viewers and other local log consumers.
