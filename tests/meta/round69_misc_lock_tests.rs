//! Round-69 lock tests for source-tree drift that doesn't fit the
//! markdown-walker bucket.
//!
//! - **F6** `silt fmt --check` printed "recursively formatting..." in
//!   its no-files-specified banner, which lied about what the command
//!   was doing. The wording now pivots on `check_mode`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── F6 ──────────────────────────────────────────────────────────────

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

/// Build an isolated temp directory holding one badly-formatted .silt
/// file, plus a `silt.toml` so `silt fmt` finds a project anchor and
/// doesn't refuse to recurse.
fn scratch_with_bad_silt() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_round69_misc_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("silt.toml"),
        "[package]\nname = \"r69\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    // Extra whitespace + unindented body: parses fine but the formatter
    // would rewrite. Forces fmt --check to actually walk the tree.
    fs::write(dir.join("bad.silt"), "fn  main( ) {\nprintln(\"hi\")\n}\n").unwrap();
    dir
}

#[test]
fn fmt_check_message_uses_check_wording() {
    let dir = scratch_with_bad_silt();
    let output = silt_cmd()
        .current_dir(&dir)
        .args(["fmt", "--check"])
        .output()
        .expect("failed to run silt fmt --check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains("checking"),
        "expected `silt fmt --check` no-files-specified banner to say \
         `checking` (not `formatting`); got stdout:\n{stdout}\n\
         stderr:\n{stderr}"
    );
    assert!(
        !combined.contains("recursively formatting"),
        "`silt fmt --check` no-files-specified banner must NOT say \
         `recursively formatting` — that lies about what `--check` \
         does. Got stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
