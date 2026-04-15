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

## What lives here vs. inline `#[cfg(test)]`

- **Here:** anything that exercises public behavior, especially across
  multiple crates. The "does the universe still hum" tests.
- **Inline in the source crate:** tests that need access to private
  items, or that exist purely to make a failure debuggable at a module
  boundary. Load-bearing only.

Inline tests are entropy. Keep them when they earn their keep; nuke
them when a regress test already has the system covered.

## Running

```sh
cargo test -p chaos-regress
```

Or as part of the full QA gate:

```sh
just qa
```
