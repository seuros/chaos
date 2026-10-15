# Real durable HTTP daemon drill

Run from the workspace root:

```sh
cargo build -p chaos-cli -p chaos_journald --no-default-features --bins
cargo test -p chaos-cli --no-default-features --test durable_httpd -- --nocapture
```

The test runs the actual `chaos serve` executable, native kernel, shell tool,
and separate journald executable against a fresh SQLite database. A loopback
SSE fixture replaces only the remote provider. All settings, model metadata,
credentials, working files, and child environments live in an isolated temporary
directory. It neither reads operator credentials nor contacts a real provider.

The assertions cover one continuous session:

- Admit a turn, hold the native provider stream, steer the active turn, then
  check that the subsequent provider request includes the steering text.
- Execute a harmless shell append and verify it occurred exactly once.
- Read invocation-local receipts and verify the exact-byte terminal webhook
  HMAC independently from the production sender.
- Start a real sleeping shell subprocess, interrupt the turn, and observe that
  native cleanup has actually terminated that subprocess.
- SIGKILL and reap the daemon while a provider response remains in flight.
- Wait for the real, storage-clock execution lease to expire, without rewriting
  the ledger or shortening production lease settings.
- Restart in recovery-only mode, retain the unknown invocation, and require
  explicit operator containment confirmation for audited resolution. An exact
  resolution retry must return the same audit; no provider request or completed
  shell side effect may be replayed.

Expect roughly 90–100 seconds, chiefly the deliberately real lease expiry.
Build journald first: CLI integration tests build `chaos`, but Cargo does not
automatically rebuild dependency packages' executable targets.

Validated on macOS arm64 on 2026-10-15: the executable-level drill passed in
91.96 seconds; the dedicated CLI integration target also passed clippy with
`-D warnings`.

## What this does not prove

The shell tool uses `--sandbox root-access` so the test can observe native
subprocess lifecycle without requiring a platform sandbox helper. This is **not
an OS-isolation test**. The sleeping subprocess is explicitly interrupted and
observed dead before the daemon crash. The test does not claim SIGKILL alone
contains arbitrary detached children, remote jobs, MCP servers, or provider
effects. Such work still needs an independently verified container/cgroup/job
boundary before an operator confirms containment.

PostgreSQL fencing, writer failures, and delivery retries have separate
journald/HTTP durability matrix tests. This executable-level drill uses SQLite;
it is not evidence of PostgreSQL process-kill behavior or complete billing.
