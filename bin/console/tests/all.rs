// Single integration test binary that aggregates all test modules.
// The submodules live in `tests/suite/`.
#[allow(unused_imports)]
use chaos_boot as _; // Keep dev-dep for cargo-shear; tests spawn the chaos binary.

mod suite;
