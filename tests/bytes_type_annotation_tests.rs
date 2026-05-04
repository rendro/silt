//! Round 72 GAP G1: lock tests for `Bytes`, `TcpListener`, and
//! `TcpStream` as type annotations.
//!
//! Background. Round 65 added these names to `BUILTIN_TYPES`
//! (`src/types/builtins.rs:156-170`) so the trait-impl-target gate
//! accepted `trait T for Bytes { ... }`. The same change did NOT
//! extend `resolve_type_expr_inner`'s `Named` arm in
//! `src/typechecker/mod.rs:4072-4101`, which only enumerates
//! Int/Float/ExtFloat/Bool/String/Unit/List/Range/Map/Set/Channel.
//! As a result, an annotation site like
//!
//! ```silt
//! import bytes
//! fn main() {
//!   let x: Bytes = bytes.empty()
//!   println(x)
//! }
//! ```
//!
//! failed to typecheck with `error[type]: unknown type 'Bytes'` —
//! despite `bytes.empty()`'s scheme already producing
//! `Type::Generic("Bytes", vec![])` and despite `trait T for Bytes`
//! being accepted as a target. Same shape for `TcpListener` and
//! `TcpStream`.
//!
//! These tests exercise the runtime via the `silt` CLI (mirroring the
//! pattern in `tests/aliased_import_runtime_tests.rs`). They fail
//! before the fix in `src/typechecker/mod.rs` (the new `Named`-arm
//! match arms for `Bytes`, `TcpListener`, `TcpStream`) and pass after.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_bytes_anno_{label}.silt"));
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

/// Run via `silt check` instead of `run` for cases where typecheck
/// alone is the lock (e.g. `TcpListener` annotations the test cannot
/// produce a value for without opening a real socket).
fn check_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_bytes_anno_chk_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("check")
        .arg(&tmp)
        .output()
        .expect("spawn silt check");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

fn check_silt_ok(label: &str, src: &str) {
    let (stdout, stderr, ok) = check_silt_raw(label, src);
    assert!(
        ok,
        "silt check should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
}

// ── Runtime locks: let-binding annotations ────────────────────────────

/// The canonical G1 repro: `let x: Bytes = bytes.empty()` must
/// typecheck and run. Pre-fix this failed with
/// `error[type]: unknown type 'Bytes'`.
#[test]
fn let_binding_bytes_annotation_runs() {
    let out = run_silt_ok(
        "let_bytes",
        r#"
import bytes
fn main() {
  let x: Bytes = bytes.empty()
  println(x)
}
"#,
    );
    // The exact display of an empty Bytes value is implementation
    // detail; the lock is that the program runs at all. Sanity-check
    // that the printed line carries the canonical "bytes" tag.
    assert!(
        out.to_lowercase().contains("bytes"),
        "expected the printed empty-bytes value to mention 'bytes'; got {out:?}"
    );
}

// ── Runtime locks: function parameter / return-type annotations ───────

/// `fn handle(b: Bytes) -> Int { 1 }` is the second flavour of the
/// finding — annotation-as-parameter, not annotation-as-let. Same
/// `Named`-arm path, so the same fix covers it; this test pins the
/// behaviour separately so a partial revert is caught.
#[test]
fn fn_param_bytes_annotation_runs() {
    let out = run_silt_ok(
        "fn_param_bytes",
        r#"
import bytes
fn handle(b: Bytes) -> Int { 1 }
fn main() {
  let x: Bytes = bytes.empty()
  println(handle(x))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "1",
        "expected handle(bytes.empty()) to print '1'; got {out:?}"
    );
}

// ── Typecheck-only locks: TcpListener / TcpStream annotations ─────────

/// `fn handle(l: TcpListener) -> Int` must typecheck. We use
/// `silt check` rather than `silt run` because actually constructing
/// a `TcpListener` requires opening a socket, which is hostile to
/// CI. The lock is on the typechecker `Named`-arm, not the runtime.
#[test]
fn fn_param_tcp_listener_annotation_typechecks() {
    check_silt_ok(
        "fn_param_tcp_listener",
        r#"
import tcp
fn handle(l: TcpListener) -> Int { 1 }
fn main() {
  println("ok")
}
"#,
    );
}

/// Companion lock for `TcpStream`. Same shape as the `TcpListener`
/// case. Without the round 72 fix both names produced
/// `error[type]: unknown type 'TcpStream'`.
#[test]
fn fn_param_tcp_stream_annotation_typechecks() {
    check_silt_ok(
        "fn_param_tcp_stream",
        r#"
import tcp
fn handle(s: TcpStream) -> Int { 1 }
fn main() {
  println("ok")
}
"#,
    );
}

/// Combined annotation site — both names in one signature plus a
/// `Bytes` parameter. Catches a partial-fix that handles only some
/// of the three opaque builtins.
#[test]
fn fn_param_combined_tcp_and_bytes_annotations_typecheck() {
    check_silt_ok(
        "fn_combined_tcp_bytes",
        r#"
import tcp
import bytes
fn handle(l: TcpListener, s: TcpStream, b: Bytes) -> Int { 1 }
fn main() {
  println("ok")
}
"#,
    );
}

// ── Negative lock: undeclared opaque-style name still errors ──────────

/// Defence in depth — an unknown uppercase name must still produce
/// the round-60 B3 "unknown type" diagnostic. The fix only adds
/// three explicit arms; it must not accidentally swallow other
/// uppercase names by widening the match too far.
#[test]
fn unknown_uppercase_type_still_errors() {
    let (_stdout, stderr, ok) = check_silt_raw(
        "unknown_uppercase",
        r#"
fn handle(x: BogusOpaque) -> Int { 1 }
fn main() {
  println("ok")
}
"#,
    );
    assert!(!ok, "silt check should reject unknown uppercase type");
    assert!(
        stderr.contains("unknown type 'BogusOpaque'"),
        "expected `unknown type 'BogusOpaque'` diagnostic; got stderr={stderr:?}"
    );
}
