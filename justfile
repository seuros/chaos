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

# Build the chaos binary (debug profile).
build *args:
    cargo build --bin chaos {{args}}

# Install chaos into ~/.cargo/bin (release + target-cpu=native).
install:
    #!/usr/bin/env sh
    set -e
    if ! command -v protoc >/dev/null 2>&1 && [ -z "${PROTOC:-}" ]; then
        echo "error: protoc not found on PATH and PROTOC is unset." >&2
        echo "" >&2
        echo "chaos pulls in rama-grpc whose build script needs protoc." >&2
        echo "" >&2
        case "$(uname -s)" in
            FreeBSD) echo "Install with: pkg install protobuf" >&2 ;;
            Darwin)  echo "Install with: brew install protobuf" >&2 ;;
            Linux)
                if command -v pacman >/dev/null 2>&1; then
                    echo "Install with: sudo pacman -S protobuf" >&2
                elif command -v apt >/dev/null 2>&1; then
                    echo "Install with: sudo apt install protobuf-compiler" >&2
                elif command -v dnf >/dev/null 2>&1; then
                    echo "Install with: sudo dnf install protobuf-compiler" >&2
                else
                    echo "Install the 'protobuf' package for your distro." >&2
                fi
                ;;
            *) echo "Install the 'protobuf' package for your OS." >&2 ;;
        esac
        echo "" >&2
        echo "Or point PROTOC at an existing binary:" >&2
        echo "  export PROTOC=/path/to/protoc" >&2
        exit 1
    fi
    RUSTFLAGS="-C target-cpu=native" cargo install --path bin/chaos --locked --force

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

# Run the bounded Postgres validation set for the new storage path.
postgres-validate-storage database_url:
    TEST_DATABASE_URL="{{database_url}}" cargo test -p chaos-storage postgres_ -- --nocapture

postgres-validate-cron database_url:
    TEST_DATABASE_URL="{{database_url}}" cargo test -p chaos-cron postgres_ -- --nocapture

postgres-validate-new-storage-path database_url:
    TEST_DATABASE_URL="{{database_url}}" cargo test -p chaos-storage postgres_ -- --nocapture
    TEST_DATABASE_URL="{{database_url}}" cargo test -p chaos-cron postgres_ -- --nocapture

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
