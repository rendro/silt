//! Round 92 regression: string interpolations with more than 255
//! segments must be rejected at compile time.
//!
//! `src/compiler/mod.rs::ExprKind::StringInterp` counts segments in a
//! `u8` and emits that count as the `Op::StringConcat` operand. Before
//! the fix the counter was unguarded: a 300-segment interpolation
//! overflow-panicked the compiler in debug builds and silently wrapped
//! mod 256 in release builds — the VM then popped `count % 256` values
//! (44 for 300 segments; ZERO for exactly 256, yielding an empty
//! string plus 256 orphaned stack values) and produced garbled output.
//!
//! The fix adds a `CompileError` guard mirroring the sibling
//! u8-operand guards (function arity, call argc, tuple/record arity).
//! These tests exercise the REAL compile path via the `silt` CLI:
//!
//!   1. 300 segments → compile error naming the count and the 255 cap.
//!   2. Exactly 256 segments (the empty-string wrap case) → same error.
//!   3. Exactly 255 segments → still compiles AND RUNS, printing the
//!      correct 255-character result (execution-path lock so the guard
//!      can't be tightened off-by-one).

use std::process::Command;

/// Run a Silt source program via the CLI and return (stdout, stderr, success).
fn run_silt(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round92_interp_{}_{}.silt",
        std::process::id(),
        label
    ));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// Build a program whose `main` interpolates `"{x}"` repeated `n` times.
fn interp_program(n: usize) -> String {
    let segments = "{x}".repeat(n);
    format!("fn main() {{\n  let x = 7\n  let s = \"{segments}\"\n  println(s)\n}}\n")
}

/// 300 segments: must be a compile error, not a debug-build panic /
/// release-build wrap. Before the fix the debug compiler aborted with
/// "attempt to add with overflow" at the `count += 1` site.
#[test]
fn interp_over_255_segments_is_compile_error() {
    let (stdout, stderr, ok) = run_silt("seg300", &interp_program(300));
    assert!(
        !ok,
        "expected silt run to fail for 300-segment interpolation; stdout={stdout:?}"
    );
    assert!(
        stderr.contains("string interpolation has 300 segments"),
        "expected segment-count compile error, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("limited to 255"),
        "expected the 255 cap in the message, got stderr: {stderr}"
    );
    assert!(
        !stderr.contains("panicked"),
        "compiler must not panic, got stderr: {stderr}"
    );
}

/// Exactly 256 segments — the nastiest wrap (count % 256 == 0): the VM
/// would have popped zero values and produced an empty string with 256
/// orphaned stack values. Must be the same compile error.
#[test]
fn interp_exactly_256_segments_is_compile_error() {
    let (stdout, stderr, ok) = run_silt("seg256", &interp_program(256));
    assert!(
        !ok,
        "expected silt run to fail for 256-segment interpolation; stdout={stdout:?}"
    );
    assert!(
        stderr.contains("string interpolation has 256 segments"),
        "expected segment-count compile error, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("limited to 255"),
        "expected the 255 cap in the message, got stderr: {stderr}"
    );
}

/// Execution-path lock: exactly 255 segments is within the u8 operand
/// budget and must keep compiling AND running, printing 255 copies of
/// the interpolated value. Guards against an off-by-one tightening of
/// the new check.
#[test]
fn interp_exactly_255_segments_still_runs() {
    let (stdout, stderr, ok) = run_silt("seg255", &interp_program(255));
    assert!(
        ok,
        "expected silt run to succeed for 255-segment interpolation; stderr={stderr}"
    );
    let expected = "7".repeat(255);
    assert_eq!(
        stdout.lines().next().unwrap_or(""),
        expected,
        "255-segment interpolation must print 255 copies of the value"
    );
}
