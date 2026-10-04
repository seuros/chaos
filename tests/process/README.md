# Shared child-process fixtures

`chaos-test-process` embeds the workspace's vendored Lua (`mlua`). Execution,
PTY, and OS-sandbox tests can run real child processes without requiring an
installed Python, Ruby, or Lua interpreter.

```sh
cargo build -p chaos-test-process
target/debug/chaos-test-process --eval 'print("hello")'
target/debug/chaos-test-process --interactive
```

The CLI is wrapper-independent: a wrapper only needs to forward argv, stdin,
stdout, stderr, and exit status. For example:

```sh
bash -c 'exec "$@"' fixture-wrapper \
  target/debug/chaos-test-process --eval 'print("hello")'
```

Rust tests use `chaos_test_process::command(script)` for an argv vector, or
`shell_command(script)` / `interactive_shell_command(script)` for correctly
quoted shell-tool commands. The binary is located or built by `chaos-which`;
missing fixtures are test failures, not silently skipped coverage.

Lua's standard `io`, `os`, and `string` libraries provide script I/O. The REPL
keeps state across input lines, prints `> `, and accepts `exit()`. Output is
unbuffered in both pipe and terminal modes.

The `fixture` table exposes native probes: `pid`, `sleep`, `stdin_isatty`,
`getpwuid`, `openpty_roundtrip`, `fork_semaphore`, `socket`, and
`spawn_detached_sleep`. macOS additionally exposes `lsopen_allowed`.
`http_get(url, use_proxy, timeout_seconds)` uses Rama's HTTP/1 implementation,
honors proxy/no-proxy environment variables, and sends absolute-form GETs to
HTTP proxies. It supports plaintext HTTP fixtures, not general-purpose HTTPS.

These are **trusted test scripts**, not user hooks. The fixture intentionally
keeps Lua's standard libraries available; it does not replace the production
hook sandbox. Native probes execute in the child so OS sandbox enforcement,
terminal mode, inherited descriptors, and process groups are still tested.
