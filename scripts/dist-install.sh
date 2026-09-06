#!/bin/sh
set -eu
DEST=${1:-$HOME/.local/bin}
BINARIES="chaos alcatraz chaos_journald chaos-forkve-wrapper"

# Reject an incomplete bundle before replacing any installed binary.
for name in $BINARIES; do
    if [ ! -f "$name" ] || [ -L "$name" ]; then
        echo "error: release archive is missing a regular file: $name" >&2
        exit 1
    fi
done
mkdir -p "$DEST"
for name in $BINARIES; do
    install -m 755 "$name" "$DEST/$name"
done

echo "Installed to $DEST"
echo "Make sure $DEST is in your PATH."
