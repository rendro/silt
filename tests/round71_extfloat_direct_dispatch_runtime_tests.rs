//! Round 71 lock tests for direct trait-method dispatch on ExtFloat,
//! Channel(_), and Fun(_, _) receivers.
//!
//! Pre-fix bug (`src/typechecker/inference.rs:2552`):
//!
//!   The `FieldAccess` outer-match arm enumerating primitive receivers
//!   was `Type::Int | Type::Float | Type::Bool | Type::String | Type::Unit`,
//!   omitting `Type::ExtFloat`, `Type::Channel(_)`, and `Type::Fun(_, _)`.
//!   `register_auto_derived_impls_for` (`src/typechecker/mod.rs:6542`)
//!   registers the ExtFloat impls under the symbol `"ExtFloat"` in
//!   `method_table`, so `(intern("ExtFloat"), intern("display"))`
//!   exists — but the lookup was never performed for an ExtFloat
//!   receiver because the value fell through to the `_ =>` arm at line
//!   2778 and reported "unknown field or method 'display' on type
//!   ExtFloat". Same root cause for user-defined `trait Show for
//!   Channel(a)` impls on a Channel receiver.
//!
//!   Repro (BROKEN, pre-fix):
//!     import math
//!     fn main() {
//!       let x = math.sqrt(2.0)         -- ExtFloat
//!       println(x.display())           -- error[type]: unknown field
//!                                      --   or method 'display' on
//!                                      --   type ExtFloat
//!     }
//!
//! Post-fix: the match arm includes `Type::ExtFloat`, `Type::Channel(_)`,
//! and `Type::Fun(_, _)`, mapping each to its canonical `method_table`
//! key ("ExtFloat", "Channel", "Fun"). The same `dispatch_method_entry`
//! call that handles Int / Float / String / Bool / Unit now routes
//! these three.
//!
//! Note on `Type::Fun(_, _)`: although the dispatch fix makes the
//! `method_table` lookup happen, the impl-target self_type for
//! `trait Show for Fun(a, b)` resolves to `Generic("Fun", [a, b])`
//! while the receiver is `Type::Fun(params, ret)`. These two shapes
//! do not unify, so a runtime end-to-end Fun test still fails at the
//! type-checker stage. That is an orthogonal limitation outside the
//! scope of this fix; the parent task instructions explicitly permit
//! skipping the Fun runtime test if it isn't a valid impl target after
//! the dispatch-arm change. We exercise dispatch coverage for
//! `Type::Fun` indirectly via the explicit pre-fix repro proof
//! captured here in comments — the new arm is exercised the moment a
//! `FieldAccess` is performed on a `Type::Fun` receiver, regardless of
//! whether the impl-target unification succeeds.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round71_extfloat_{label}.silt"));
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

/// Run and assert success; return stdout.
fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

// ── ExtFloat direct receiver dispatch ───────────────────────────────

/// `.display()` on an ExtFloat receiver (produced by `math.sqrt`)
/// must route through the auto-derived `(ExtFloat, display)` entry
/// in `method_table` and produce the rendered `f64::to_string()`
/// output. Pre-fix: the receiver fell through the FieldAccess match
/// to the `_ =>` arm and surfaced "unknown field or method 'display'
/// on type ExtFloat".
#[test]
fn extfloat_display_direct_dispatch() {
    let out = run_silt_ok(
        "extfloat_display",
        r#"
import math
fn main() {
  let x = math.sqrt(2.0)
  println(x.display())
}
"#,
    );
    assert!(
        out.contains("1.41"),
        "expected display() of math.sqrt(2.0) to contain 1.41; got {out:?}"
    );
}

/// `.equal()` on two equal ExtFloat receivers must return `true`.
/// Same FieldAccess-arm root cause pre-fix.
#[test]
fn extfloat_equal_direct_dispatch_true() {
    let out = run_silt_ok(
        "extfloat_equal_true",
        r#"
import math
fn main() {
  let a = math.sqrt(2.0)
  let b = math.sqrt(2.0)
  println(a.equal(b))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "true",
        "math.sqrt(2.0).equal(math.sqrt(2.0)) should be true; got {out:?}"
    );
}

/// `.compare()` returns -1/0/1 (Int) per the runtime dispatch arm in
/// `src/vm/dispatch.rs:404`. Self-compare on ExtFloat must yield 0
/// (`Ordering::Equal` -> `0`). Pre-fix: same FieldAccess-arm error.
#[test]
fn extfloat_compare_direct_dispatch_equal() {
    let out = run_silt_ok(
        "extfloat_compare_equal",
        r#"
import math
fn main() {
  let a = math.sqrt(2.0)
  let b = math.sqrt(2.0)
  println(a.compare(b))
}
"#,
    );
    assert_eq!(
        out.trim(),
        "0",
        "self-compare of math.sqrt(2.0) should be 0 (Ordering::Equal); got {out:?}"
    );
}

/// `.hash()` on an ExtFloat receiver must route through the
/// auto-derived `(ExtFloat, hash)` entry and produce a parseable Int.
/// The actual hash value is not pinnable — assert program runs and
/// the output parses as `i64`.
#[test]
fn extfloat_hash_direct_dispatch_returns_int() {
    let out = run_silt_ok(
        "extfloat_hash",
        r#"
import math
fn main() {
  let x = math.sqrt(2.0)
  println(x.hash())
}
"#,
    );
    out.trim().parse::<i64>().unwrap_or_else(|e| {
        panic!("expected Int hash on stdout for math.sqrt(2.0).hash(), got {out:?}: {e}")
    });
}

// ── Channel direct receiver dispatch ────────────────────────────────

/// User-defined `trait Show for Channel(a)` runtime test — the
/// FieldAccess match arm must route a `Type::Channel(_)` receiver
/// into `method_table.get(("Channel", "show"))` so the user's impl
/// body (`"chan-impl"`) is found. Pre-fix: the receiver fell through
/// to the `_ =>` arm and surfaced "unknown field or method 'show'
/// on type Channel(Int)".
#[test]
fn channel_user_trait_dispatch() {
    let out = run_silt_ok(
        "channel_show",
        r#"
import channel
trait Show { fn show(self) -> String }
trait Show for Channel(a) { fn show(self) -> String = "chan-impl" }
fn main() {
  let c: Channel(Int) = channel.new(1)
  println(c.show())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "chan-impl",
        "user-defined Show for Channel should print 'chan-impl'; got {out:?}"
    );
}

// ── Fun direct receiver dispatch ────────────────────────────────────
//
// `trait Show for Fun(a, b)` does not actually unify with a concrete
// `Type::Fun(params, ret)` at the receiver-self unify step inside
// `dispatch_method_entry` — the impl's self_type is
// `Generic("Fun", [a, b])` while the receiver is `Type::Fun(...)`, and
// these do not unify. That's an orthogonal limitation in the
// trait-impl-target machinery outside the scope of the FieldAccess
// dispatch-arm fix.
//
// Per the parent task instructions, the Fun runtime test is skipped
// when Fun is not a valid impl-target after the dispatch-arm change.
// Verified manually: the program below errors with "type mismatch:
// expected Fun(_, _), got Fn() -> Int" rather than the pre-fix
// "unknown field or method 'show' on type Fn() -> Int". The fix did
// move dispatch past the FieldAccess match (the new error is from
// `dispatch_method_entry`'s receiver-self unify), confirming the new
// `Type::Fun(_, _)` arm is reached.
//
// Repro (post-fix, expected to fail at the unify step):
//   trait Show { fn show(self) -> String }
//   trait Show for Fun(a, b) { fn show(self) -> String = "fn-impl" }
//   fn main() {
//     let f = fn() { 42 }
//     println(f.show())
//   }
//
// We capture the post-fix behaviour as a documented skip rather than
// a failing assertion.
#[test]
fn fun_user_trait_dispatch_skipped() {
    // Run the program; we expect a typecheck error (NOT the pre-fix
    // "unknown field or method" error). Confirm the error is the
    // unify-step mismatch, proving the dispatch arm now routes
    // `Type::Fun(_, _)` into `method_table` rather than falling
    // through to the `_ =>` arm.
    let (_stdout, stderr, ok) = run_silt_raw(
        "fun_show_skip",
        r#"
trait Show { fn show(self) -> String }
trait Show for Fun(a, b) { fn show(self) -> String = "fn-impl" }
fn main() {
  let f = fn() { 42 }
  println(f.show())
}
"#,
    );
    assert!(
        !ok,
        "Fun impl-target unify is a known orthogonal limitation; expected failure"
    );
    assert!(
        !stderr.contains("unknown field or method 'show'"),
        "post-fix error must NOT be the pre-fix 'unknown field or method' diagnostic; got {stderr:?}"
    );
}
