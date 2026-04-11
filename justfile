set working-directory := "."
set positional-arguments

# Display help
help:
    just -l

# Run chaos (debug build)
alias c := chaos
chaos *args:
    cargo run --bin chaos -- {{args}}

# Run chaos with max optimization: release profile (fat LTO, single
# codegen unit, stripped symbols) plus `-C target-cpu=native` so the
# build uses every SIMD extension the local CPU advertises. Unix-only
# project, so portable codegen is wasted on a daily-driver binary.
alias b := bigbang
bigbang *args:
    RUSTFLAGS="-C target-cpu=native" cargo run --release --bin chaos -- {{args}}

# Format code
fmt:
    cargo fmt

# Clippy with all features, deny warnings
clippy:
    cargo clippy --workspace --all-features --tests -- -D warnings

# Check compilation without building
check:
    cargo check --workspace --all-targets --all-features

# Run tests with all features
test *args:
    cargo nextest run --workspace --all-features --no-fail-fast {{args}}

# Lint + check + clippy (no tests)
qq: fmt check clippy

# Full QA: qq + tests
qa: qq test

# Fix clippy warnings automatically
fix:
    cargo clippy --fix --workspace --all-features --tests --allow-dirty

# Run the MCP server
mcp-server-run *args:
    cargo run -p chaos-mcpd -- {{args}}

# Write hooks JSON schema fixtures
[no-cd]
write-hooks-schema:
    cargo run -p chaos-dtrace --bin write_hooks_schema_fixtures

