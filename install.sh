#!/bin/sh
# Install script for silt
# Usage: curl -fsSL https://raw.githubusercontent.com/rendro/silt/main/install.sh | sh

set -eu

REPO="rendro/silt"
INSTALL_DIR="${SILT_INSTALL_DIR:-$HOME/.local/bin}"
BASE_URL="https://github.com/$REPO/releases"

main() {
    os="$(detect_os)"
    arch="$(detect_arch)"
    target="${arch}-${os}"

    if [ "$os" = "pc-windows-msvc" ]; then
        ext="zip"
        bin="silt.exe"
    else
        ext="tar.gz"
        bin="silt"
    fi

    version="$(get_latest_version)"
    if [ -z "$version" ]; then
        err "could not determine latest version"
    fi

    asset="silt-${version}-${target}.${ext}"
    url="$BASE_URL/download/$version/$asset"
    sums_url="$BASE_URL/download/$version/silt-${version}-SHA256SUMS"

    printf "  Installing silt %s (%s)\n" "$version" "$target"
    printf "  Target: %s\n\n" "$INSTALL_DIR/$bin"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    printf "  Downloading %s\n" "$url"
    fetch "$url" "$tmpdir/silt.$ext"

    printf "  Downloading %s\n" "$sums_url"
    fetch "$sums_url" "$tmpdir/SHA256SUMS" \
        || err "failed to download SHA256SUMS — refusing to install unverified binary"

    printf "  Verifying SHA-256 checksum\n"
    verify_sha256 "$tmpdir/silt.$ext" "$tmpdir/SHA256SUMS" "$asset" \
        || err "checksum verification failed — refusing to install"

    printf "  Extracting\n"
    if [ "$ext" = "tar.gz" ]; then
        tar xzf "$tmpdir/silt.$ext" -C "$tmpdir"
    else
        unzip -q "$tmpdir/silt.$ext" -d "$tmpdir"
    fi

    mkdir -p "$INSTALL_DIR"
    cp "$tmpdir/$bin" "$INSTALL_DIR/$bin"
    chmod +x "$INSTALL_DIR/$bin"

    printf "\n  silt %s installed to %s\n\n" "$version" "$INSTALL_DIR/$bin"

    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        printf "  Note: %s is not in your PATH.\n" "$INSTALL_DIR"
        printf "  Add it with:\n\n"
        printf "    export PATH=\"%s:\$PATH\"\n\n" "$INSTALL_DIR"
    fi

    printf "  LSP: the prebuilt binary includes the language server.\n"
    printf "  Run 'silt lsp' and see https://github.com/%s/tree/main/editors for editor setup.\n\n" "$REPO"
}

detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "unknown-linux-gnu" ;;
        Darwin*) echo "apple-darwin" ;;
        MINGW*|MSYS*|CYGWIN*) echo "pc-windows-msvc" ;;
        *) err "unsupported OS: $(uname -s)" ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) err "unsupported architecture: $(uname -m)" ;;
    esac
}

fetch() {
    # fetch <url> <dest> — exits non-zero if the download fails so callers
    # can fail closed on missing files (e.g. SHA256SUMS).
    _url="$1"
    _dest="$2"
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL "$_url" -o "$_dest"
    elif command -v wget > /dev/null 2>&1; then
        wget -q "$_url" -O "$_dest"
    else
        err "need curl or wget to download"
    fi
}

verify_sha256() {
    # verify_sha256 <file> <SHA256SUMS> <asset-name>
    # Extract the hash for <asset-name>, recompute from <file>, bail on
    # mismatch. Accepts both "hash  name" (sha256sum) and "hash *name"
    # (binary-mode) entries, tolerates comment/blank lines.
    _file="$1"
    _sums="$2"
    _asset="$3"
    _expected="$(awk -v a="$_asset" '
        /^[[:space:]]*#/ { next }
        NF == 0 { next }
        {
            name = $2
            sub(/^\*/, "", name)
            if (name == a) { print $1; exit }
        }
    ' "$_sums")"
    if [ -z "$_expected" ]; then
        printf "  error: no SHA256SUMS entry for %s\n" "$_asset" >&2
        return 1
    fi
    if command -v sha256sum > /dev/null 2>&1; then
        _actual="$(sha256sum "$_file" | awk '{print $1}')"
    elif command -v shasum > /dev/null 2>&1; then
        _actual="$(shasum -a 256 "$_file" | awk '{print $1}')"
    else
        printf "  error: need sha256sum or shasum to verify download\n" >&2
        return 1
    fi
    if [ "$_expected" != "$_actual" ]; then
        printf "  error: sha256 mismatch for %s\n" "$_asset" >&2
        printf "    expected %s\n" "$_expected" >&2
        printf "    got      %s\n" "$_actual" >&2
        return 1
    fi
    return 0
}

get_latest_version() {
    # GitHub's /releases/latest endpoint 302-redirects to /releases/tag/<version>.
    # We fetch only the response headers (no -L) and parse the Location header
    # case-insensitively, then take the final path segment as the version.
    if command -v curl > /dev/null 2>&1; then
        curl -fsI "$BASE_URL/latest" 2>/dev/null \
            | awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); n = split($2, parts, "/"); print parts[n]; exit }'
    elif command -v wget > /dev/null 2>&1; then
        wget --server-response --spider --max-redirect=0 "$BASE_URL/latest" 2>&1 \
            | awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); n = split($2, parts, "/"); print parts[n]; exit }'
    fi
}

err() {
    printf "  error: %s\n" "$1" >&2
    exit 1
}

main
