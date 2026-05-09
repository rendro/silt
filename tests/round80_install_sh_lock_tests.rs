//! Source-grep regression locks for round-80 install.sh fixes.
//!
//! Finding G2 (round 80): the unpack and install steps following the
//! round-79 fetch fixes (`tar`/`unzip`/`mkdir`/`cp`/`chmod`) still
//! relied solely on `set -eu` for failure handling, producing raw tool
//! errors with no `error: <msg>` branding. Round 79 added `|| err
//! "..."` to the binary-fetch and SHA256SUMS-fetch lines; round 80
//! extends the same fallback pattern to the post-download steps so a
//! corrupt archive or read-only install dir surfaces as a branded
//! actionable error.
//!
//! These tests read install.sh as plain text and assert each of the
//! `tar`, `unzip`, `mkdir -p`, `cp`, and `chmod +x` lines is followed
//! (on the same line or via a `\` continuation) by `|| err`.

const INSTALL_SH: &str = include_str!("../install.sh");

/// Returns the index of the first line whose content matches `pred`,
/// along with the line itself plus the next line (joined by a space)
/// so callers can match `|| err` whether it sits on the same line or
/// on a continuation chained by trailing `\`.
fn locate<F: Fn(&str) -> bool>(pred: F) -> Option<(usize, String, String)> {
    let lines: Vec<&str> = INSTALL_SH.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if pred(l) {
            let joined = format!("{} {}", lines[i], lines.get(i + 1).unwrap_or(&""));
            return Some((i, lines[i].to_string(), joined));
        }
    }
    None
}

fn assert_has_err_fallback(label: &str, locator: impl Fn(&str) -> bool) {
    let (i, raw, joined) = locate(locator)
        .unwrap_or_else(|| panic!("could not locate {} line in install.sh", label));
    assert!(
        joined.contains("|| err"),
        "install.sh {} step at line {} lacks `|| err` fallback (G2): {}",
        label,
        i + 1,
        raw
    );
}

#[test]
fn install_sh_tar_extract_has_err_fallback() {
    // The `tar xzf` extract on the .tar.gz path must chain `|| err`
    // so a corrupt download surfaces a branded error instead of an
    // opaque `set -eu` exit.
    assert_has_err_fallback("tar extract", |l| {
        l.trim_start().starts_with("tar ") && l.contains("xzf")
    });
}

#[test]
fn install_sh_unzip_extract_has_err_fallback() {
    // Mirror lock for the Windows .zip path.
    assert_has_err_fallback("unzip extract", |l| {
        l.trim_start().starts_with("unzip ")
    });
}

#[test]
fn install_sh_mkdir_install_dir_has_err_fallback() {
    // `mkdir -p $INSTALL_DIR` must produce a branded error when the
    // user's install dir is read-only or unwritable.
    assert_has_err_fallback("mkdir install dir", |l| {
        l.trim_start().starts_with("mkdir ") && l.contains("$INSTALL_DIR")
    });
}

#[test]
fn install_sh_cp_binary_has_err_fallback() {
    // `cp` of the binary into the install dir must produce a branded
    // error on permission failure.
    assert_has_err_fallback("cp binary", |l| {
        l.trim_start().starts_with("cp ") && l.contains("$INSTALL_DIR")
    });
}

#[test]
fn install_sh_chmod_binary_has_err_fallback() {
    // `chmod +x` on the installed binary must produce a branded error
    // on permission failure.
    assert_has_err_fallback("chmod binary", |l| {
        l.trim_start().starts_with("chmod ") && l.contains("$INSTALL_DIR")
    });
}
