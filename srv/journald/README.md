# chaos-journald

Append-only session journal service. Receives rollout items from the kernel over
a Unix socket and persists them to SQLite. The authoritative record of what
happened in a process — session resume and history replay read from here.
