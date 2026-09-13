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
