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
//! key ("ExtFloat", "Channel", "Fn" — round 71 follow-up canonicalised
//! function-type sites onto "Fn"). The same `dispatch_method_entry`
//! call that handles Int / Float / String / Bool / Unit now routes
//! these three.
//!
//! The round 71 follow-up (TYPE-3 LATENT) closed the original
//! `Type::Fun(_, _)` end-to-end limitation: `canonicalize_type_name`
//! collapses `Fun → Fn`, the unifier accepts
//! `Type::Fun(_, _) ↔ Generic("Fn", [])`, and the runtime dispatch
//! name returned by `dispatch_name_for_value(VmClosure)` is now `"Fn"`.
//! `fn_user_trait_dispatch_runtime` below exercises the end-to-end
//! flow.

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

// ── Fn direct receiver dispatch ─────────────────────────────────────
//
// Round 71 originally documented `fun_user_trait_dispatch_skipped` as
// a deferred limitation: the FieldAccess match arm was extended to
// route `Type::Fun(_, _)` into the `method_table`, but the impl's
// self_type (`Generic("Fun", [...])` then) did not unify with a
// concrete `Type::Fun(...)` receiver, so end-to-end dispatch still
// failed at the unify step.
//
// The round 71 follow-up (TYPE-3 LATENT) closed that gap by
// canonicalising every function-type-name dispatch site on `"Fn"`
// (`canonical_name`, `head_symbol_of_canon`, `type_name_for_impl`,
// `dispatch_name_for_value`, the FieldAccess primitive-dispatch key)
// AND adding two unifier-side enablers:
//   - `canonicalize_type_name` collapses `Fun → Fn` so `trait T for
//     Fun` and `trait T for Fn` register under one key.
//   - The unifier accepts `Type::Fun(_, _) ↔ Generic("Fn", [])` so
//     bare-`Fn` impls (parser rejects `Fn(...) -> ...` as a
//     parameterised impl target — variadic, no surface form) unify
//     with any function-shaped receiver.
//
// The end-to-end repro below now succeeds. The full lock test suite
// for the canonical-name unification lives in
// `tests/round71_followup_fn_canonical_name_tests.rs`.
#[test]
fn fn_user_trait_dispatch_runtime() {
    let out = run_silt_ok(
        "fn_show_runtime",
        r#"
trait Show { fn show(self) -> String }
trait Show for Fn { fn show(self) -> String = "fn-impl" }
fn main() {
  let f = fn() { 42 }
  println(f.show())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "fn-impl",
        "user-defined Show for Fn should now dispatch to the impl body \
         after the round 71 follow-up canonical-name unification; got {out:?}"
    );
}
