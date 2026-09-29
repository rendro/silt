//! Round-100 BROKEN: `silt test` silently dropped all but the LAST
//! positional path.
//!
//! `silt test a_test.silt b_test.silt` ran ONLY `b_test.silt`, reported
//! `1 test: 1 passed`, and exited 0 — `a_test.silt` never ran but the run
//! read green. In CI a test file that is silently skipped looks like a
//! pass: a real hazard. `silt run` / `silt check` / `silt disasm` all
//! already reject extra positionals; `test` was the lone outlier (its
//! `else` arm was an unconditional last-wins `file = Some(...)`).
//!
//! Fix mirrors `silt check`: accept at most one positional (a file or a
//! directory to scan) and reject the rest with exit 1.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_r100_test_gate_{n}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn silt_test_rejects_multiple_positional_files() {
    let dir = fresh_dir();
    // Two trivially-passing test files. Pre-fix, only the second ran.
    fs::write(dir.join("a_test.silt"), "fn test_a() { let x = 1 }\n").unwrap();
    fs::write(dir.join("b_test.silt"), "fn test_b() { let x = 2 }\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["test", "a_test.silt", "b_test.silt"])
        .current_dir(&dir)
        .output()
        .expect("spawn silt test");

    assert!(
        !out.status.success(),
        "`silt test a_test.silt b_test.silt` must FAIL rather than \
         silently run only the second file.\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("silt test") && stderr.contains("extra argument"),
        "stderr must explain the rejection; got: {stderr}"
    );
}

#[test]
fn silt_test_still_accepts_a_single_file() {
    // The gate must not break the legitimate one-positional form.
    let dir = fresh_dir();
    fs::write(dir.join("only_test.silt"), "fn test_only() { let x = 1 }\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["test", "only_test.silt"])
        .current_dir(&dir)
        .output()
        .expect("spawn silt test");
    assert!(
        out.status.success(),
        "single-file `silt test only_test.silt` must still run.\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
