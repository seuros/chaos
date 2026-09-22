#!/usr/bin/env bash
set -euo pipefail

binary="$1"
shift
run_dir="$(dirname -- "$binary")/../nextest-ui"
run_binary="$run_dir/$(basename -- "$binary")"
mkdir -p "$run_dir"
if [[ ! "$binary" -ef "$run_binary" ]]; then
    staging="$(mktemp -d "$run_dir/.stage.XXXXXX")"
    trap 'rm -rf -- "$staging"' EXIT
    ln -- "$binary" "$staging/test"
    mv -f -- "$staging/test" "$run_binary"
    rm -rf -- "$staging"
    trap - EXIT
fi

exec "$run_binary" "$@"
