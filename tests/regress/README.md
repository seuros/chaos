# chaos-regress

> One test suite to rule them all, one suite to find them,
> one suite to bring them all, and in the entropy bind them.

The system-level test crate. Every test that proves chaos behaves as a
whole — not just that a function returns the right tuple — lives here.

## Layout

```
tests/regress/
  Cargo.toml          # workspace member, dev-deps on crates under test
  src/lib.rs          # empty (no library, just a host for integration tests)
  tests/
    uptime.rs         # one file per crate under test
    ...
```

Each file in `tests/` compiles as its own integration-test binary and
sees only the **public API** of the crates it depends on. If a test
needs to peek at private state, it's the wrong test.

## What lives here vs. source-crate unit tests

- **Here:** anything that exercises public behavior, especially across
  multiple crates. The "does the universe still hum" tests.
- **In the source crate:** tests that need access to private
  items, or that exist purely to make a failure debuggable at a module
  boundary. Load-bearing only. Substantial suites live in `<module>/tests.rs`
  as test-only child modules; small blocks can remain inline.

Keep source-crate tests when they earn their keep; remove them only
when a surviving test covers the same behavior and conditions, per the
[contribution guidelines](../../docs/contributing.md#invariants-and-error-handling).

## Running

```sh
cargo test -p chaos-regress
```

Or as part of the full QA gate:

```sh
just qa
```

### Claude clamp continuation

`clamp_resume` runs three separate `chaos exec` processes against a scripted
Claude peer and a private journald. The peer launches the actual session MCP
bridge and calls real file/shell tools; only the provider's native history is
faked. The regression checks delta-only continuation, retention of a tool-only
result after its source is deleted, fresh reads, and no repeated audit writes.
It requires neither Claude nor provider credentials.

The same scenario can exercise an installed, authenticated Claude CLI:

```sh
cargo build --bin chaos --bin chaos_journald
CHAOS_CLAMP_SMOKE=1 cargo test -p chaos-regress --test clamp_resume -- --ignored --nocapture
```

Build those binaries before a focused deterministic run too; the full
workspace test gate builds them already.

This opt-in smoke test makes paid model calls. Tools are instructed to operate
only in its temporary workspace, under `--full-auto`'s workspace-write sandbox.
The scripted peer uses headless execution for its fixed, test-owned commands
so default CI does not depend on platform sandbox support.
The tests check behavior, not exact cache counters or model phrasing.
The deterministic test belongs in CI; the live test checks compatibility with
the actual Claude CLI's native session implementation.

### Antigravity clamp continuation

The same `clamp_resume` suite also drives a scripted Antigravity peer through
three separate `chaos exec` processes and actual MCP file/shell calls. It checks
native ID reuse, tool-only memory, fresh reads, and exact audit writes, then
injects an error after a real write and verifies one dispatch, no renewed
checkpoint, and a fresh explicit retry. Only native model behavior is faked;
no credentials or provider network access are required. This is not a live
Antigravity CLI compatibility result.

Shared checkpoint unit tests own history/context compatibility, consume-before-
dispatch, backend identity, old-format rejection, private storage and all-unsent
hook/system-message deltas. The obsolete last-user/MCP-only selector is removed;
its non-text and instruction-separation coverage now tests rendered native input.
