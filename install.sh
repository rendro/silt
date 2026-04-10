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

    url="$BASE_URL/download/$version/silt-${version}-${target}.${ext}"

    printf "  Installing silt %s (%s)\n" "$version" "$target"
    printf "  Target: %s\n\n" "$INSTALL_DIR/$bin"

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    printf "  Downloading %s\n" "$url"
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL "$url" -o "$tmpdir/silt.$ext"
    elif command -v wget > /dev/null 2>&1; then
        wget -q "$url" -O "$tmpdir/silt.$ext"
    else
        err "need curl or wget to download"
    fi

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
