#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
case "$(uname -s)" in
    Darwin) default_tmp_root="/private/tmp/chaos-qa-${USER:-user}/tmp" ;;
    *) default_tmp_root="/tmp/chaos-qa-${USER:-user}/tmp" ;;
esac
tmp_root="${CHAOS_QA_TMPDIR:-$default_tmp_root}"

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
export DARWIN_USER_TEMP_DIR="$TMPDIR"
export DARWIN_USER_CACHE_DIR="${DARWIN_USER_CACHE_DIR:-$repo_root/.tmp/qa/cache}"

# Keep the default QA path hermetic. Runtime-storage environment variables are
# useful for manual Postgres validation, but letting them leak into the general
# unit/integration suite makes temp CHAOS_HOME fixtures share the operator's
# runtime database. The justfile has dedicated postgres-* recipes for tests that
# intentionally opt into external storage.
unset CHAOS_STORAGE_URL
unset CHAOS_SQLITE_HOME

mkdir -p "$DARWIN_USER_CACHE_DIR"

exec "$@"
