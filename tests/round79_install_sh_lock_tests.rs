//! Source-grep regression locks for round-79 install.sh fixes.
//!
//! Finding DX-G2 (GAP): the `fetch "$url" "$tmpdir/silt.$ext"` call that
//! downloads the actual binary lacked a `|| err "..."` fallback, while
//! the sibling SHA256SUMS fetch *did* have one. Without this lock the
//! install script would silently `set -eu`-bail with an opaque error
//! when GitHub returns a non-zero status for the binary URL — a broken
//! UX that prompted this round.
//!
//! This test reads install.sh as plain text and asserts the binary
//! fetch line is followed by `|| err`.

const INSTALL_SH: &str = include_str!("../install.sh");

#[test]
fn install_sh_binary_fetch_has_err_fallback() {
    // The binary fetch must chain `|| err` like the SHA256SUMS fetch
    // does — finding DX-G2, round 79.
    let lines: Vec<&str> = INSTALL_SH.lines().collect();
    let mut binary_line_idx: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.contains("fetch \"$url\"") && l.contains("/silt.") {
            binary_line_idx = Some(i);
            break;
        }
    }
    let i = binary_line_idx.expect("could not locate binary fetch line in install.sh");
    // The `|| err` may be on the same line, or on a continuation line
    // chained by a trailing `\` — accept both forms.
    let line_or_continuation = format!("{} {}", lines[i], lines.get(i + 1).unwrap_or(&""));
    assert!(
        line_or_continuation.contains("|| err"),
        "install.sh binary fetch at line {} lacks `|| err` fallback (DX-G2): {}",
        i + 1,
        lines[i]
    );
}

#[test]
fn install_sh_sha256sums_fetch_has_err_fallback() {
    // Sibling lock for the SHA256SUMS fetch — this has always had the
    // `|| err` fallback, but locking it here prevents silent removal.
    let lines: Vec<&str> = INSTALL_SH.lines().collect();
    let mut sums_line_idx: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.contains("fetch \"$sums_url\"") {
            sums_line_idx = Some(i);
            break;
        }
    }
    let i = sums_line_idx.expect("could not locate SHA256SUMS fetch line in install.sh");
    let line_or_continuation = format!("{} {}", lines[i], lines.get(i + 1).unwrap_or(&""));
    assert!(
        line_or_continuation.contains("|| err"),
        "install.sh SHA256SUMS fetch at line {} lacks `|| err` fallback: {}",
        i + 1,
        lines[i]
    );
}
