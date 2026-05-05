//! Round-73 audit regressions for `silt` CLI help / discoverability.
//!
//! Three DX gaps the audit caught:
//!
//! - **G2**: `silt --help` previously listed all 11 subcommands but
//!   silently omitted the recognized global flags (`--version`/`-V`/`-v`,
//!   `--help`/`-h`). Users had no way to discover them from the banner.
//!   Lock that the global-flags section now advertises `--version` and
//!   `--help`.
//!
//! - **G3**: typos like `silt --verison` fell through to the unknown-
//!   subcommand arm without a "did you mean" suggestion because the
//!   suggestion pool only contained subcommand strings (none starting
//!   with `-`). Lock that flag-shaped typos now surface a flag
//!   suggestion via the same edit-distance machinery.
//!
//! - **G4**: `silt help <subcommand>` previously printed the global
//!   banner instead of drilling into the per-subcommand help (the
//!   convention every cargo / git / rustup user expects). Lock that
//!   `silt help fmt` now produces the same content as
//!   `silt fmt --help`.
//!
//! All three are CLI-surface bugs, so the lock tests exercise the
//! binary via `Command::new(env!("CARGO_BIN_EXE_silt"))` per the
//! round-73 audit requirement.

use std::process::Command;

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

// ── G2: silt --help advertises global flags ────────────────────────

#[test]
fn silt_help_lists_version_global_flag() {
    // Pre-fix `silt --help` had a Usage section listing 11 subcommand
    // rows but no mention of `--version` / `-V` / `-v` even though
    // `src/main.rs` recognized those tokens. The banner now grows a
    // "Global flags:" section so the recognized flags are discoverable
    // from the top-level help.
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    assert!(
        output.status.success(),
        "silt --help must exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--version"),
        "silt --help must list --version flag, got:\n{stdout}"
    );
    // The grouping header is part of the contract too — without it the
    // flag rows would visually blur into the subcommand rows.
    assert!(
        stdout.contains("Global flags"),
        "silt --help must label the global-flags section, got:\n{stdout}"
    );
}

#[test]
fn silt_help_lists_help_global_flag() {
    // Symmetric lock for `--help` itself — listing the discovery flag
    // in the banner guides the user to `silt help <cmd>` for
    // per-subcommand drill-in.
    let output = silt_cmd()
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--help"),
        "silt --help must list --help flag in the banner, got:\n{stdout}"
    );
}

// ── G3: typo'd global flags surface a "did you mean" suggestion ────

#[test]
fn silt_verison_typo_suggests_version_flag() {
    // Pre-fix the unknown-subcommand arm only searched the SUBCOMMANDS
    // const, which has no entries starting with `-`, so a flag typo
    // routed to "Unknown command" with no hint. `silt chek` correctly
    // suggested `check`; `silt --verison` should now do the same for
    // `--version`.
    let output = silt_cmd()
        .arg("--verison")
        .output()
        .expect("failed to run silt --verison");
    assert!(
        !output.status.success(),
        "silt --verison must exit non-zero, got {:?}",
        output.status
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--version"),
        "silt --verison stderr must reference '--version', got: {stderr}"
    );
    assert!(
        stderr.contains("Did you mean"),
        "silt --verison stderr must surface a 'Did you mean' hint, got: {stderr}"
    );
}

#[test]
fn silt_hep_typo_suggests_help_flag() {
    // Symmetric lock for `--help`. Edit distance `--hep` → `--help`
    // is 1, well within the threshold of 2 used by suggest_subcommand.
    let output = silt_cmd()
        .arg("--hep")
        .output()
        .expect("failed to run silt --hep");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--help"),
        "silt --hep stderr must reference '--help', got: {stderr}"
    );
    assert!(
        stderr.contains("Did you mean"),
        "silt --hep stderr must surface a 'Did you mean' hint, got: {stderr}"
    );
}

// ── G4: `silt help <subcommand>` drills into per-subcommand help ───

#[test]
fn silt_help_fmt_matches_silt_fmt_help() {
    // Cargo / git / rustup all treat `<bin> help <cmd>` as a synonym
    // for `<bin> <cmd> --help`. Pre-fix `silt help fmt` printed the
    // global banner, ignoring the trailing `fmt` argument. Now it
    // re-dispatches as `silt fmt --help` so the two paths produce the
    // same output.
    let via_help = silt_cmd()
        .args(["help", "fmt"])
        .output()
        .expect("failed to run silt help fmt");
    let via_flag = silt_cmd()
        .args(["fmt", "--help"])
        .output()
        .expect("failed to run silt fmt --help");
    assert!(
        via_help.status.success(),
        "silt help fmt must exit 0, got {:?}",
        via_help.status
    );
    assert!(
        via_flag.status.success(),
        "silt fmt --help must exit 0, got {:?}",
        via_flag.status
    );
    let via_help_out = String::from_utf8_lossy(&via_help.stdout).into_owned();
    let via_flag_out = String::from_utf8_lossy(&via_flag.stdout).into_owned();
    assert_eq!(
        via_help_out, via_flag_out,
        "silt help fmt and silt fmt --help must produce identical stdout.\n\
         help fmt: {via_help_out}\n\
         fmt --help: {via_flag_out}"
    );
    // Defense-in-depth: assert the marker that's specific to
    // `silt fmt --help` (not the global banner) actually shows up.
    // The fmt help banner starts with "Usage: silt fmt", which the
    // global banner never contains.
    assert!(
        via_help_out.starts_with("Usage: silt fmt"),
        "silt help fmt must drill into the fmt-specific banner, got:\n{via_help_out}"
    );
}

#[test]
fn silt_help_with_no_arg_still_shows_global_banner() {
    // Bare `silt help` (no trailing subcommand) keeps its prior
    // behavior of printing the global usage banner, exit 0. The G4
    // routing only kicks in when there is a subcommand argument.
    let output = silt_cmd()
        .arg("help")
        .output()
        .expect("failed to run silt help");
    assert!(
        output.status.success(),
        "silt help (no arg) must exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage:"),
        "silt help must print the global Usage banner, got:\n{stdout}"
    );
    // The global banner has the 'Enabled features:' footer; the
    // per-subcommand banners don't. This distinguishes the two.
    assert!(
        stdout.contains("Enabled features:"),
        "silt help must print the global banner (not a per-subcommand one), got:\n{stdout}"
    );
}

#[test]
fn silt_help_check_drills_into_check_help() {
    // Lock the routing for a second subcommand so the implementation
    // doesn't accidentally hard-code `fmt`.
    let via_help = silt_cmd()
        .args(["help", "check"])
        .output()
        .expect("failed to run silt help check");
    let via_flag = silt_cmd()
        .args(["check", "--help"])
        .output()
        .expect("failed to run silt check --help");
    assert!(via_help.status.success());
    assert!(via_flag.status.success());
    let via_help_out = String::from_utf8_lossy(&via_help.stdout).into_owned();
    let via_flag_out = String::from_utf8_lossy(&via_flag.stdout).into_owned();
    assert_eq!(
        via_help_out, via_flag_out,
        "silt help check must produce same output as silt check --help"
    );
}
