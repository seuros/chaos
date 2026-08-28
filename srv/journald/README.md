# chaos_journald

Append-only session journal storage. SQLite receives rollout items through the
journald Unix-socket sidecar. PostgreSQL-backed ChaOS processes use the mounted
PostgreSQL pool directly, without starting or contacting journald. Session resume
and history replay read from the active storage backend.
