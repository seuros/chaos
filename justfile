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
    os=$(uname -s)
    hint_pkg() {
        # $@ carries one arg per non-empty package; word-split by shell.
        [ $# -eq 0 ] && return 0
        case "$os" in
            FreeBSD) echo "Install with: pkg install $*" >&2 ;;
            Darwin)  echo "Install with: brew install $*" >&2 ;;
            Linux)
                if command -v pacman >/dev/null 2>&1; then
                    echo "Install with: sudo pacman -S $*" >&2
                elif command -v apt >/dev/null 2>&1; then
                    echo "Install with: sudo apt install $*" >&2
                elif command -v dnf >/dev/null 2>&1; then
                    echo "Install with: sudo dnf install $*" >&2
                else
                    echo "Install ($*) with your distro package manager." >&2
                fi
                ;;
            *) echo "Install ($*) with your OS package manager." >&2 ;;
        esac
    }
    # Package name mapping by OS / package manager.
    #   protobuf -> protoc binary (rama-grpc build script).
    #   clang    -> libclang + resource headers (rama-dns via bindgen).
    #   pkgconf  -> pkg-config binary (libdbus-sys build script).
    #   dbus     -> libdbus-1 headers + shared library (libdbus-sys link).
    case "$os" in
        Linux)
            if command -v apt >/dev/null 2>&1; then
                protobuf_pkg=protobuf-compiler
                clang_pkg=libclang-dev
                pkgconf_pkg=pkg-config
                dbus_pkg=libdbus-1-dev
            elif command -v dnf >/dev/null 2>&1; then
                protobuf_pkg=protobuf-compiler
                clang_pkg=clang-devel
                pkgconf_pkg=pkgconf-pkg-config
                dbus_pkg=dbus-devel
            else
                protobuf_pkg=protobuf
                clang_pkg=clang
                pkgconf_pkg=pkgconf
                dbus_pkg=dbus
            fi
            ;;
        FreeBSD)
            protobuf_pkg=protobuf
            clang_pkg=$(pkg rquery -x '%n' '^llvm[0-9]+$' 2>/dev/null | sort -V | tail -1)
            [ -z "$clang_pkg" ] && clang_pkg=llvm
            pkgconf_pkg=pkgconf
            dbus_pkg=dbus
            ;;
        Darwin)
            protobuf_pkg=protobuf
            clang_pkg=llvm
            pkgconf_pkg=pkgconf
            dbus_pkg=dbus
            ;;
        *)
            protobuf_pkg=protobuf
            clang_pkg=clang
            pkgconf_pkg=pkgconf
            dbus_pkg=dbus
            ;;
    esac
    # FreeBSD base ships /usr/bin/clang but none of its resource headers,
    # so auto-pick the newest installed llvm<N> port and point the build
    # at it before we assess whether clang headers are reachable.
    if [ "$os" = "FreeBSD" ] && [ -z "${LIBCLANG_PATH:-}" ]; then
        # Prefer versioned stable llvm<N> ports; only fall through to
        # llvm-devel / llvm-lite when no numbered port is installed.
        candidates=$(ls -d /usr/local/llvm[0-9]*/lib 2>/dev/null | sort -V -r)
        [ -z "$candidates" ] && candidates=$(ls -d /usr/local/llvm*/lib 2>/dev/null | sort -V -r)
        for lib_dir in $candidates; do
            if [ -f "$lib_dir/libclang.so" ]; then
                export LIBCLANG_PATH="$lib_dir"
                bin_dir="$(dirname "$lib_dir")/bin"
                [ -x "$bin_dir/clang" ] && export PATH="$bin_dir:$PATH"
                break
            fi
        done
    fi
    missing=
    need_protobuf_pkg=
    need_clang_pkg=
    need_pkgconf_pkg=
    need_dbus_pkg=
    if ! command -v protoc >/dev/null 2>&1 && [ -z "${PROTOC:-}" ]; then
        missing="${missing}protoc "
        need_protobuf_pkg=$protobuf_pkg
    fi
    clang_ok=1
    if command -v clang >/dev/null 2>&1; then
        resource_dir=$(clang -print-resource-dir 2>/dev/null || true)
        if [ -z "$resource_dir" ] || [ ! -f "$resource_dir/include/stddef.h" ]; then
            clang_ok=0
        fi
    else
        clang_ok=0
    fi
    if [ "$clang_ok" -eq 0 ]; then
        missing="${missing}libclang "
        need_clang_pkg=$clang_pkg
    fi
    if ! command -v pkg-config >/dev/null 2>&1 && ! command -v pkgconf >/dev/null 2>&1; then
        missing="${missing}pkg-config "
        need_pkgconf_pkg=$pkgconf_pkg
    fi
    # Probe for dbus-1 via pkg-config if it's available; otherwise assume
    # missing on Unix hosts (macOS arboard uses NSPasteboard, so skip).
    if [ "$os" != "Darwin" ]; then
        if command -v pkg-config >/dev/null 2>&1; then
            pkg-config --exists dbus-1 2>/dev/null || { missing="${missing}dbus-1 "; need_dbus_pkg=$dbus_pkg; }
        elif command -v pkgconf >/dev/null 2>&1; then
            pkgconf --exists dbus-1 2>/dev/null || { missing="${missing}dbus-1 "; need_dbus_pkg=$dbus_pkg; }
        else
            missing="${missing}dbus-1 "
            need_dbus_pkg=$dbus_pkg
        fi
    fi
    if [ -n "$missing" ]; then
        echo "error: missing build prerequisites: $missing" >&2
        echo "" >&2
        echo "chaos pulls in rama-grpc (protoc), rama-dns (libclang," >&2
        echo "via bindgen), and arboard (pkg-config + libdbus-1 for" >&2
        echo "the clipboard backend on Linux and FreeBSD)." >&2
        echo "" >&2
        set --
        [ -n "$need_protobuf_pkg" ] && set -- "$@" "$need_protobuf_pkg"
        [ -n "$need_clang_pkg" ]    && set -- "$@" "$need_clang_pkg"
        [ -n "$need_pkgconf_pkg" ]  && set -- "$@" "$need_pkgconf_pkg"
        [ -n "$need_dbus_pkg" ]     && set -- "$@" "$need_dbus_pkg"
        hint_pkg "$@"
        echo "" >&2
        echo "Or point the build at existing binaries:" >&2
        [ -n "$need_protobuf_pkg" ] && echo "  export PROTOC=/path/to/protoc" >&2
        [ -n "$need_clang_pkg" ]    && echo "  export LIBCLANG_PATH=/path/to/libclang/lib" >&2
        [ -n "$need_pkgconf_pkg" ]  && echo "  ensure pkg-config is on PATH" >&2
        [ -n "$need_dbus_pkg" ]     && echo "  ensure PKG_CONFIG_PATH points at a directory with dbus-1.pc" >&2
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
