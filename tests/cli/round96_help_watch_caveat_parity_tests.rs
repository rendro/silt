//! Regression test for round-96 GAP — `silt --help` watch-caveat parity.
//!
//! On a no-`watch` build the `run` row appends a "  [--watch requires
//! feature: watch]" caveat to its description (so the user is warned BEFORE
//! invoking a `--watch` flag that the build can't honor), but the `check`,
//! `disasm`, and `test` rows historically advertised a bare `[--watch]` in
//! their signatures with NO such caveat. A user on a no-watch build would
//! see `[--watch]` for those three subcommands, invoke e.g. `silt check
//! --watch f.silt`, and hit the no-watch gate in `src/cli/watch.rs` that
//! exits 1 with "The 'watch' feature is not enabled."
//!
//! The test reads `silt --help` from the binary it was built with: every
//! row advertising `[--watch]` must carry the caveat exactly when the
//! build lacks the `watch` feature.

use std::process::Command;

const CAVEAT: &str = "[--watch requires feature: watch]";

#[test]
fn every_watch_row_carries_the_caveat_exactly_on_no_watch_builds() {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("--help")
        .output()
        .expect("failed to run silt --help");
    assert!(out.status.success(), "silt --help must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let watch_rows: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with("silt ") && l.contains("[--watch]"))
        .collect();
    // run, check, test, disasm.
    assert_eq!(
        watch_rows.len(),
        4,
        "expected exactly 4 `[--watch]` usage rows (run/check/test/disasm):\n{stdout}"
    );

    let want_caveat = !cfg!(feature = "watch");
    for row in watch_rows {
        assert_eq!(
            row.contains(CAVEAT),
            want_caveat,
            "`[--watch]` row {row:?} must {} the caveat {CAVEAT:?} on this build",
            if want_caveat { "carry" } else { "not carry" }
        );
    }
}
