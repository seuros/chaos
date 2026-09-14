#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
    echo "usage: $0 TARGET TAG" >&2
    exit 2
fi
target=$1
tag=$2
for value in "$target" "$tag"; do
    case "$value" in
        ''|*[!a-zA-Z0-9._-]*) echo "invalid target or tag: $value" >&2; exit 2 ;;
    esac
done
cd "$(dirname "$0")/.."
release_dir="${CARGO_TARGET_DIR:-target}/${target}/release"
bundle=$(mktemp -d)
trap 'rm -rf "$bundle"' EXIT

checksum() {
    local digest
    if command -v sha256sum >/dev/null 2>&1; then
        digest=$(sha256sum "$1")
    elif command -v shasum >/dev/null 2>&1; then
        digest=$(shasum -a 256 "$1")
    else
        digest=$(sha256 -q "$1")
    fi
    printf '%s  %s\n' "${digest%% *}" "$1"
}

# Build helpers once, separately from the CLI's feature selection.
cargo build --release --locked --target "$target" \
    -p alcatraz -p chaos_journald -p chaos-doas \
    --bin alcatraz --bin chaos_journald --bin chaos-forkve-wrapper
for bin in alcatraz chaos_journald chaos-forkve-wrapper; do
    cp "$release_dir/$bin" "$bundle/$bin"
    strip "$bundle/$bin" 2>/dev/null || true
done
cp scripts/dist-install.sh "$bundle/install.sh"

for flavor in tui headless; do
    features=(--no-default-features)
    suffix=
    if [[ "$flavor" == tui ]]; then
        features+=(--features tui)
    else
        suffix=-headless
    fi
    # Select only chaos-cli so workspace feature unification cannot restore the TUI.
    cargo build --release --locked --target "$target" \
        -p chaos-cli --bin chaos "${features[@]}"
    cp "$release_dir/chaos" "$bundle/chaos"
    strip "$bundle/chaos" 2>/dev/null || true

    help=$(NO_COLOR=1 "$bundle/chaos" --help)
    if [[ ( "$flavor" == tui && "$help" != *--no-alt-screen* ) ||
          ( "$flavor" == headless && "$help" == *--no-alt-screen* ) ]]; then
        echo "$flavor build has the wrong CLI feature surface" >&2
        exit 1
    fi
    artifact="chaos${suffix}-${tag}-${target}.tar.gz"
    COPYFILE_DISABLE=1 tar czf "$artifact" -C "$bundle" .
    checksum "$artifact" > "$artifact.sha256"
done
