//! Source-grep regression lock for round-87 install.sh mktemp fix.
//!
//! Finding (round 87, DX-LATENT): the `tmpdir="$(mktemp -d)"` line in
//! install.sh relied solely on `set -eu` for failure handling. If
//! `mktemp -d` fails (e.g. `/tmp` is read-only or `$TMPDIR` is
//! misconfigured) the script exits with the raw shell error and no
//! `error: <msg>` branding, breaking parity with the surrounding
//! steps that all chain `|| err "..."` (download, checksum verify,
//! extract, mkdir, cp, chmod — see round-79/80 locks).
//!
//! Round 87 appends `|| err "mktemp -d failed"` to the line so users
//! get a branded actionable error instead of a bare exit. This test
//! reads install.sh as plain text and asserts the line containing
//! `mktemp -d` also contains `|| err`.

const INSTALL_SH: &str = include_str!("../install.sh");

#[test]
fn install_sh_mktemp_d_has_err_fallback() {
    // Locate the `mktemp -d` line and confirm it (or its continuation)
    // also contains `|| err` so a failed temp-dir creation surfaces as
    // a branded error rather than an opaque `set -eu` exit.
    let lines: Vec<&str> = INSTALL_SH.lines().collect();
    let mut found = false;
    for (i, l) in lines.iter().enumerate() {
        if l.contains("mktemp -d") {
            found = true;
            let joined = format!("{} {}", lines[i], lines.get(i + 1).unwrap_or(&""));
            assert!(
                joined.contains("|| err"),
                "install.sh mktemp -d step at line {} lacks `|| err` fallback: {}",
                i + 1,
                l
            );
        }
    }
    assert!(
        found,
        "could not locate `mktemp -d` line in install.sh — has the script been restructured?"
    );
}
