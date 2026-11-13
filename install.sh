#!/bin/sh
# Install script for silt
# Usage: curl -fsSL https://silt-lang.com/install.sh | sh

set -eu

REPO="rendro/silt"
INSTALL_DIR="${SILT_INSTALL_DIR:-$HOME/.local/bin}"
BASE_URL="https://github.com/$REPO/releases"

main() {
    # The tool that downloads, chosen once: curl, or a wget that can be
    # kept to https.
    DOWNLOADER="$(downloader)" || exit 1
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

    tmpdir="$(mktemp -d)" || err "mktemp -d failed"
    trap 'rm -rf "$tmpdir"' EXIT
    # A signal ends the script through `exit`, so that the EXIT trap
    # runs (dash runs none when a signal ends it).
    trap 'exit 130' INT TERM HUP

    printf "  Downloading %s\n" "$url"
    fetch "$url" "$tmpdir/silt.$ext" \
        || err "failed to download binary from $url (HTTP fetch returned non-zero)"

    printf "  Downloading %s\n" "$sums_url"
    fetch "$sums_url" "$tmpdir/SHA256SUMS" \
        || err "failed to download SHA256SUMS — refusing to install unverified binary"

    printf "  Verifying SHA-256 checksum\n"
    verify_sha256 "$tmpdir/silt.$ext" "$tmpdir/SHA256SUMS" "$asset" \
        || err "checksum verification failed — refusing to install"

    # The checksum comes from the same release as the archive; the
    # attestation is signed by the release workflow, so it also catches a
    # release whose files were replaced. It needs the GitHub CLI.
    attested=0
    verify_attestation "$tmpdir/silt.$ext" "$tmpdir/attestation.out" || attested=$?
    case "$attested" in
        0) printf "  Verified the build attestation\n" ;;
        1)
            cat "$tmpdir/attestation.out" >&2
            err "the build attestation of $asset does not verify — refusing to install"
            ;;
        *)
            printf "  Build attestation not checked: that needs gh 2.49 or later, logged in to github.com\n"
            ;;
    esac

    printf "  Extracting\n"
    if [ "$ext" = "tar.gz" ]; then
        tar xzf "$tmpdir/silt.$ext" -C "$tmpdir" \
            || err "failed to extract archive (tar returned non-zero — corrupted download?)"
    else
        unzip -q "$tmpdir/silt.$ext" -d "$tmpdir" \
            || err "failed to extract archive (unzip returned non-zero — corrupted download?)"
    fi

    mkdir -p "$INSTALL_DIR" \
        || err "failed to create install dir $INSTALL_DIR (permission denied?)"
    if [ -d "$INSTALL_DIR/$bin" ]; then
        err "$INSTALL_DIR/$bin is a directory: remove it, or install elsewhere with SILT_INSTALL_DIR"
    fi
    # The new binary is copied beside the old one and then moved onto
    # its name: a running silt (an editor's `silt lsp`) keeps running,
    # and an interrupted install leaves the old binary whole.
    staged="$INSTALL_DIR/.$bin.new.$$"
    trap 'rm -rf "$tmpdir"; rm -f "$staged"' EXIT
    cp "$tmpdir/$bin" "$staged" \
        || err "failed to install binary to $INSTALL_DIR (permission denied?)"
    # Readable and runnable by everyone, as a binary in a bin directory
    # is, whatever the umask made of the copy.
    chmod 755 "$staged" \
        || err "failed to mark $INSTALL_DIR/$bin executable (permission denied?)"
    mv -f "$staged" "$INSTALL_DIR/$bin" \
        || err "failed to replace $INSTALL_DIR/$bin (permission denied?)"

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
    # Only over https, a redirect included.
    if [ "$DOWNLOADER" = curl ]; then
        curl --proto '=https' --tlsv1.2 -fsSL "$_url" -o "$_dest"
    else
        wget_https "$_url" "$_dest"
    fi
}

downloader() {
    # downloader: prints `curl` or `wget`, the tool that downloads. A wget
    # must know --max-redirect (GNU wget does; busybox's does not), as it
    # is kept to https by following each redirect itself.
    if command -v curl > /dev/null 2>&1; then
        echo curl
    elif command -v wget > /dev/null 2>&1; then
        if wget --help 2>&1 | grep -q -e '--max-redirect'; then
            echo wget
        else
            err "this wget cannot be kept to https (it has no --max-redirect, as busybox's wget): install curl or GNU wget"
        fi
    else
        err "need curl or wget to download"
    fi
}

wget_https() {
    # wget_https <url> <dest>: wget follows a redirect wherever it leads
    # (its --https-only is for recursive downloads only), so here each
    # redirect is followed by hand, and only to https.
    _wurl="$1"
    _hops=0
    while :; do
        _said="$(wget --max-redirect=0 --server-response -O "$2" "$_wurl" 2>&1)" && return 0
        # The Location of the server's answer (its headers are indented).
        _loc="$(printf '%s\n' "$_said" | awk '/^  / && tolower($1) == "location:" { l = $2 } END { sub(/\r$/, "", l); print l }')"
        _host="${_wurl#https://}"
        _host="${_host%%/*}"
        case "$_loc" in
            "") return 1 ;;
            [Hh][Tt][Tt][Pp][Ss]://*) _wurl="https://${_loc#*://}" ;;
            # On the https host it names, or on this one.
            //*) _wurl="https:$_loc" ;;
            /*) _wurl="https://$_host$_loc" ;;
            # A new query of the path asked for.
            \?*) _wurl="${_wurl%%[?#]*}$_loc" ;;
            *)
                case "${_loc%%:*}" in
                    "$_loc" | [!A-Za-z]* | *[!A-Za-z0-9+.-]* | "")
                        # A path beside the one asked for (at the root of
                        # a URL that has no path).
                        _base="${_wurl%%[?#]*}"
                        case "${_base#https://}" in
                            */*) _wurl="${_base%/*}/$_loc" ;;
                            *) _wurl="$_base/$_loc" ;;
                        esac
                        ;;
                    *)
                        printf "  error: a download was redirected to %s, which is not https\n" "$_loc" >&2
                        return 1
                        ;;
                esac
                ;;
        esac
        _hops=$((_hops + 1))
        if [ "$_hops" -gt 10 ]; then
            printf "  error: a download was redirected more than 10 times\n" >&2
            return 1
        fi
    done
}

verify_attestation() {
    # verify_attestation <file> <out>: check the build provenance that the
    # release workflow signed for <file>, with the GitHub CLI; what gh
    # says goes to <out>. 0 when it verifies, 1 when it does not, 2 when
    # it cannot be checked here: no gh, a gh without the attestation
    # command (before 2.49), or a gh not logged in to github.com.
    command -v gh > /dev/null 2>&1 || return 2
    gh attestation --help > /dev/null 2>&1 || return 2
    gh auth status --hostname github.com > /dev/null 2>&1 || return 2
    gh attestation verify "$1" --repo "$REPO" > "$2" 2>&1 || return 1
    return 0
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
    if [ "$DOWNLOADER" = curl ]; then
        curl --proto '=https' --tlsv1.2 -fsI "$BASE_URL/latest" 2>/dev/null \
            | awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); n = split($2, parts, "/"); print parts[n]; exit }'
    else
        wget --server-response --spider --max-redirect=0 "$BASE_URL/latest" 2>&1 \
            | awk 'tolower($1) == "location:" { sub(/\r$/, "", $2); n = split($2, parts, "/"); print parts[n]; exit }'
    fi
}

err() {
    printf "  error: %s\n" "$1" >&2
    exit 1
}

main
