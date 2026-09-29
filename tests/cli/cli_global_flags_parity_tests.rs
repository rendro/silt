//! Round-75 G5 — every advertised top-level flag is handled, and the
//! unknown-command path suggests the flags.
//!
//! `src/cli/help.rs` declares the recognized top-level flag tokens as
//! `GLOBAL_FLAGS`, and `run_main` in `src/main.rs` routes them. Adding
//! a new alias to one side without the other either leaves a token the
//! binary then rejects as an unknown command, or drops it from the
//! typo-suggestion pool (`silt --hlpe` would not surface `--help`).
//! These tests check both halves through the binary.

use std::process::Command;

fn silt(arg: &str) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(arg)
        .output()
        .expect("failed to run silt");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn every_global_flag_is_handled() {
    let version = format!("silt {}", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V", "-v"] {
        let (code, stdout, stderr) = silt(flag);
        assert_eq!(code, Some(0), "silt {flag}: stderr={stderr}");
        assert!(
            stdout.contains(&version),
            "silt {flag}: expected {version:?} in stdout, got: {stdout}"
        );
    }
    for flag in ["--help", "-h", "help"] {
        let (code, stdout, stderr) = silt(flag);
        assert_eq!(code, Some(0), "silt {flag}: stderr={stderr}");
        assert!(
            stdout.contains("Usage:"),
            "silt {flag}: expected usage in stdout, got: {stdout}"
        );
        assert!(
            !stderr.contains("Unknown command"),
            "silt {flag}: routed to the unknown-command arm: {stderr}"
        );
    }
}

#[test]
fn global_flags_are_offered_as_typo_suggestions() {
    for (typo, flag) in [("--hlpe", "--help"), ("--versoin", "--version")] {
        let (code, _stdout, stderr) = silt(typo);
        assert_eq!(code, Some(1), "silt {typo}: stderr={stderr}");
        assert!(
            stderr.contains("Unknown command")
                && stderr.contains(&format!("Did you mean '{flag}'?")),
            "silt {typo}: expected a suggestion of {flag}, got: {stderr}"
        );
    }
}
