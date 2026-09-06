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
        release_url="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${REPO}/releases/latest")"
        tag="${release_url##*/}"
        [ -n "$tag" ] && [ "$tag" != "latest" ] \
            || err "no stable release found for ${REPO} (publish one or set CHAOS_VERSION)"
        say "latest release: $tag"
    fi

    archive="chaos-${tag}-${target}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${tag}/${archive}"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    say "downloading $url"
    curl -fSL --progress-bar -o "$tmpdir/$archive" "$url"
    curl -fsSL -o "$tmpdir/$archive.sha256" "$url.sha256"
    verify_checksum "$tmpdir/$archive" "$tmpdir/$archive.sha256"

    tar xzf "$tmpdir/$archive" -C "$tmpdir"

    # Check the complete bundle before replacing any installed binary.
    for name in chaos alcatraz chaos_journald chaos-forkve-wrapper; do
        [ -f "$tmpdir/$name" ] && [ ! -L "$tmpdir/$name" ] \
            || err "release archive is missing a regular file: $name"
    done
    mkdir -p "$INSTALL_DIR"
    install_bin "$tmpdir/chaos" "chaos"
    install_bin "$tmpdir/alcatraz" "alcatraz"
    install_bin "$tmpdir/chaos_journald" "chaos_journald"
    install_bin "$tmpdir/chaos-forkve-wrapper" "chaos-forkve-wrapper"

    say "installed chaos to $INSTALL_DIR/chaos"

    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        say ""
        say "WARNING: $INSTALL_DIR is not in your PATH"
        say "add it with:  export PATH=\"$INSTALL_DIR:\$PATH\""
    fi
}

verify_checksum() {
    # Release assets use sha256sum's "<digest>  <filename>" format. Hash the
    # downloaded path ourselves rather than trusting paths in the manifest.
    expected="$(awk 'NR == 1 { print $1 }' "$2")"
    [ "$(wc -l < "$2" | tr -d ' ')" = 1 ] \
        || err "invalid SHA-256 manifest"
    [ "${#expected}" = 64 ] || err "invalid SHA-256 digest"
    case "$expected" in *[!0-9a-fA-F]*) err "invalid SHA-256 digest" ;; esac

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1")"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$1")"
    elif command -v sha256 >/dev/null 2>&1; then
        actual="$(sha256 -q "$1")"
    else
        err "required command not found: sha256sum, shasum, or sha256"
    fi
    actual="${actual%% *}"
    [ "$actual" = "$(printf '%s' "$expected" | tr 'A-F' 'a-f')" ] \
        || err "release archive SHA-256 mismatch; nothing installed"
    say "verified SHA-256"
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
