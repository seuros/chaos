#!/bin/sh
set -e
DEST=${1:-$HOME/.local/bin}
mkdir -p "$DEST"

install_bin() {
    if [ -f "$1" ]; then
        cp "$1" "$DEST/$1"
        chmod +x "$DEST/$1"
    else
        echo "warning: $1 not found, skipping" >&2
    fi
}

install_bin chaos
install_bin chaos_journald
install_bin chaos-forkve-wrapper
install_bin chaos-xclient

os=$(uname -s)
case "$os" in
    Linux)   ln -sf "$DEST/chaos" "$DEST/alcatraz-linux" ;;
    FreeBSD) ln -sf "$DEST/chaos" "$DEST/alcatraz-freebsd" ;;
    Darwin)  ln -sf "$DEST/chaos" "$DEST/alcatraz-macos" ;;
esac

echo "Installed to $DEST"
echo "Make sure $DEST is in your PATH."
