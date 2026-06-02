#!/bin/sh
set -eu

REPO="${CHAOS_REPO:-seuros/chaos}"
INSTALL_DIR="${CHAOS_INSTALL_DIR:-$HOME/.local/bin}"

main() {
    need_cmd curl
    need_cmd tar
    need_cmd uname
    need_cmd mktemp

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)
            case "$arch" in
                x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
                *) err "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        Darwin)
            case "$arch" in
                arm64|aarch64) target="aarch64-apple-darwin" ;;
                *) err "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        FreeBSD)
            case "$arch" in
                x86_64|amd64) target="x86_64-unknown-freebsd" ;;
                *) err "unsupported FreeBSD architecture: $arch" ;;
            esac
            ;;
        *) err "unsupported OS: $os" ;;
    esac

    say "detected target: $target"

    if [ -n "${CHAOS_VERSION:-}" ]; then
        tag="$CHAOS_VERSION"
        say "pinned release: $tag"
    else
        # /releases/latest skips prereleases; the list endpoint returns the
        # newest first (prereleases included, drafts omitted for anonymous
        # requests), so take its first tag_name.
        tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases?per_page=1" \
            | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
        [ -n "$tag" ] || err "no published release found for ${REPO} (drafts are not installable; publish one or set CHAOS_VERSION)"
        say "latest release: $tag"
    fi

    archive="chaos-${tag}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    say "downloading $url"
    curl -fSL --progress-bar -o "$tmpdir/$archive" "$url"

    tar xzf "$tmpdir/$archive" -C "$tmpdir"

    mkdir -p "$INSTALL_DIR"
    install_bin "$tmpdir/chaos" "chaos"
    install_bin "$tmpdir/chaos-journald" "chaos-journald"
    install_bin "$tmpdir/chaos-forkve-wrapper" "chaos-forkve-wrapper"
    install_bin "$tmpdir/chaos-xclient" "chaos-xclient"

    case "$os" in
        Linux)   ln -sf "$INSTALL_DIR/chaos" "$INSTALL_DIR/alcatraz-linux" ;;
        Darwin)  ln -sf "$INSTALL_DIR/chaos" "$INSTALL_DIR/alcatraz-macos" ;;
        FreeBSD) ln -sf "$INSTALL_DIR/chaos" "$INSTALL_DIR/alcatraz-freebsd" ;;
    esac

    say "installed chaos to $INSTALL_DIR/chaos"

    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        say ""
        say "WARNING: $INSTALL_DIR is not in your PATH"
        say "add it with:  export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
}

install_bin() {
    src="$1"
    name="$2"
    [ -f "$src" ] || err "release archive is missing $name"
    install -m 755 "$src" "$INSTALL_DIR/$name"
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || err "required command not found: $1"
}

say() {
    printf '%s\n' "$1"
}

err() {
    say "error: $1" >&2
    exit 1
}

main "$@"
