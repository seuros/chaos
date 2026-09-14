# chaos-pty

Lightweight helpers for spawning interactive processes either under a PTY (pseudo terminal) or regular pipes. The public API is minimal and mirrors both backends so callers can switch based on their needs (e.g., enabling or disabling TTY).

## API surface

- `spawn_pty_process(program, args, cwd, env, arg0, size)` → `SpawnedProcess`
- `spawn_pipe_process(program, args, cwd, env, arg0)` → `SpawnedProcess`
- `spawn_pipe_process_no_stdin(program, args, cwd, env, arg0)` → `SpawnedProcess`
- `combine_output_receivers(stdout_rx, stderr_rx)` → `broadcast::Receiver<Vec<u8>>`
- `conpty_supported()` → `bool` (Windows only; always true elsewhere)
- `TerminalSize { rows, cols }` selects PTY dimensions in character cells.
- `ProcessHandle` exposes:
  - `writer_sender()` → `mpsc::Sender<Vec<u8>>` (stdin)
  - `resize(TerminalSize)`
  - `close_stdin()`
  - `has_exited()`, `exit_code()`, `terminate()`
- `SpawnedProcess` bundles `session`, `stdout_rx`, `stderr_rx`, and `exit_rx` (oneshot exit code).

## Usage examples

```rust
use std::collections::HashMap;
use std::path::Path;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use codex_utils_pty::TerminalSize;

# tokio_test::block_on(async {
let env_map: HashMap<String, String> = std::env::vars().collect();
let spawned = spawn_pty_process(
    "bash",
    &["-lc".into(), "echo hello".into()],
    Path::new("."),
    &env_map,
    &None,
    TerminalSize::default(),
).await?;

let writer = spawned.session.writer_sender();
writer.send(b"exit\n".to_vec()).await?;

// Collect output until the process exits.
let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
let mut collected = Vec::new();
while let Ok(chunk) = output_rx.try_recv() {
    collected.extend_from_slice(&chunk);
}
let exit_code = spawned.exit_rx.await.unwrap_or(-1);
# let _ = (collected, exit_code);
# anyhow::Ok(())
# });
```

Swap in `spawn_pipe_process` for a non-TTY subprocess; the rest of the API stays the same.
Use `spawn_pipe_process_no_stdin` to force stdin closed (commands that read stdin will see EOF immediately).

## Tests

Unit tests in `src/process/tests.rs` exercise cancellation, stdin ownership, and
output-channel draining with an in-memory child and channels. They do not launch
subprocesses or wait for wall-clock delays:

```sh
cargo test -p chaos-pty --lib
```

Real PTY/pipe, Python REPL, inherited-FD, session, and process-group checks live
in `tests/process.rs` and use the public API:

```sh
cargo test -p chaos-pty --test process
```

Integration tests synchronize on readiness output, stdin handshakes, process exit,
and output EOF. Their timeouts are failure guards, not successful completion signals.
Python and `setsid` cases retain their existing availability checks. Both suites
remain part of `cargo test -p chaos-pty` and `just test`.
