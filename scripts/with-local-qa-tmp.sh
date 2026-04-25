#!/usr/bin/env bash
set -euo pipefail

tmp_root="${CHAOS_QA_TMPDIR:-${XDG_CACHE_HOME:-$HOME/.cache}/chaos/qa/tmp}"

case "${1:-}" in
    --print-tmp-root)
        printf '%s\n' "$tmp_root"
        exit 0
        ;;
    --clean)
        rm -rf "$tmp_root"
        exit 0
        ;;
esac

mkdir -p "$tmp_root"
export TMPDIR="$tmp_root"
export TMP="$TMPDIR"
export TEMP="$TMPDIR"

exec "$@"
