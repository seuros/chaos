#![cfg(target_os = "linux")]

// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
mod suite;
