//! Round 71 follow-up: lock tests for the `"Fn"` canonical-name
//! unification across the four function-type-name dispatch sites.
//!
//! Pre-fix state (audit TYPE-3 LATENT, round 71):
//!
//!   - `type_name_for_impl(Type::Fun)` returned `intern("Fun")`
//!     (`src/typechecker/mod.rs:1840`).
//!   - `canonical_name(Type::Fun)` returned `"Fn"`
//!     (`src/types/canonical.rs:425`).
//!   - `head_symbol_of_canon(Type::Fun)` returned `intern("Fn")`
//!     (`src/types/canonical.rs:512`).
//!   - `dispatch_name_for_value(VmClosure)` returned `"Function"`
//!     (`src/types/canonical.rs:573`).
//!   - The FieldAccess-arm dispatch lookup keyed on `intern("Fun")`
//!     (`src/typechecker/inference.rs:2575`).
//!
//!   Three different names for the same shape produced a three-way
//!   mismatch: a user `trait Show for Fn { ... }` registered under
//!   `("Show", "Fn")`, the FieldAccess lookup tried `("Fn", "show")` /
//!   `("Fun", "show")`, and the VM dispatched under `"Function.show"`,
//!   so the method was never found at runtime.
//!
//! Post-fix state (round 71 follow-up):
//!
//!   All four sites canonicalise on `"Fn"`. The fix combines:
//!   - `canonicalize_type_name` collapses surface alias `Fun → Fn` so a
//!     `trait T for Fun` impl registers under the same `("T", "Fn")`
//!     key as `trait T for Fn`.
//!   - The unifier accepts `Type::Fun(_, _) ↔ Generic("Fn", [])` so a
//!     bare `for Fn` impl unifies with any function-shaped receiver
//!     (the parser rejects `Fn(...) -> ...` as a parameterised impl
//!     target — `Fn` is variadic and has no surface form for "any
//!     function").
//!
//! Round 71's `fun_user_trait_dispatch_skipped` test (in
//! `tests/round71_extfloat_direct_dispatch_runtime_tests.rs`)
//! documented the orthogonal limitation. After this round the
//! limitation is gone — the test is replaced by an end-to-end repro
//! that asserts the user impl actually dispatches.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round71fu_fn_{label}.silt"));
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

// ── End-to-end runtime dispatch ─────────────────────────────────────

/// `trait Show for Fn { ... }` must dispatch at runtime when invoked on
/// a function value (`fn() { 42 }`). Pre-fix: the impl registered under
/// `("Show", "Fn")` while the VM dispatched under `"Function"`, so the
/// `<Type>.<method>` qualified-global lookup missed and the method body
/// was never run.
#[test]
fn user_trait_for_fn_dispatches_correctly_at_runtime() {
    let out = run_silt_ok(
        "user_show_for_fn",
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
        "user-defined Show for Fn should print 'fn-impl'; got {out:?}"
    );
}

/// Same as above but using the deprecated surface alias `Fun`. The
/// `canonicalize_type_name` collapse `Fun → Fn` makes both forms
/// register under `("Show", "Fn")` so this dispatches the same way.
/// Pre-fix: the impl registered under `("Show", "Fun")` and was
/// unreachable from the runtime `"Function.show"` (or, post-partial-fix,
/// `"Fn.show"`) lookup.
#[test]
fn user_trait_for_fun_alias_dispatches_correctly() {
    let out = run_silt_ok(
        "user_show_for_fun_alias",
        r#"
trait Show { fn show(self) -> String }
trait Show for Fun { fn show(self) -> String = "fn-impl" }
fn main() {
  let f = fn() { 42 }
  println(f.show())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "fn-impl",
        "user-defined Show for Fun (alias) should also print 'fn-impl'; got {out:?}"
    );
}

// ── Source-grep locks ───────────────────────────────────────────────

/// `dispatch_name_for_value(VmClosure)` must return `"Fn"`, not the
/// pre-fix `"Function"`. Source-grep on `src/types/canonical.rs` so a
/// future drift back to `"Function"` is caught before behaviour
/// regresses.
#[test]
fn function_value_canonical_name_is_fn() {
    let src = include_str!("../src/types/canonical.rs");
    assert!(
        src.contains(r#"Value::VmClosure(_) => Some("Fn".to_string())"#),
        "expected `dispatch_name_for_value` VmClosure arm to produce `\"Fn\"`; \
         the round 71 follow-up canonical-name unification lock is broken"
    );
    // Defence in depth: the pre-fix literal must be gone from the
    // VmClosure arm. The bare substring check would false-positive on
    // a comment elsewhere in the file, so search for the exact arm
    // shape that was the bug.
    assert!(
        !src.contains(r#"Value::VmClosure(_) => Some("Function".to_string())"#),
        "pre-fix literal `Some(\"Function\".to_string())` for VmClosure \
         must not return; canonical name is `\"Fn\"`"
    );
}

/// `type_name_for_impl(Type::Fun)` must return `intern("Fn")`, not the
/// pre-fix `intern("Fun")`. Source-grep on `src/typechecker/mod.rs`
/// catches a future drift.
#[test]
fn type_name_for_impl_returns_fn_not_fun() {
    let src = include_str!("../src/typechecker/mod.rs");
    assert!(
        src.contains(r#"Type::Fun(_, _) => Some(intern("Fn"))"#),
        "expected `type_name_for_impl` Type::Fun arm to produce `intern(\"Fn\")`; \
         the round 71 follow-up canonical-name unification lock is broken"
    );
    assert!(
        !src.contains(r#"Type::Fun(_, _) => Some(intern("Fun"))"#),
        "pre-fix literal `intern(\"Fun\")` for Type::Fun must not return"
    );
}

/// `inference.rs`'s FieldAccess-arm primitive dispatch must key
/// function-type receivers under `intern("Fn")`, matching the
/// `method_table` registration site.
#[test]
fn field_access_dispatch_keys_fn_under_fn() {
    let src = include_str!("../src/typechecker/inference.rs");
    assert!(
        src.contains(r#"Type::Fun(_, _) => intern("Fn"),"#),
        "expected FieldAccess primitive-dispatch arm to key Type::Fun \
         under `intern(\"Fn\")`; the round 71 follow-up lock is broken"
    );
    assert!(
        !src.contains(r#"Type::Fun(_, _) => intern("Fun"),"#),
        "pre-fix literal `intern(\"Fun\")` for Type::Fun dispatch key \
         must not return"
    );
}

/// `canonicalize_type_name` must collapse the deprecated surface alias
/// `Fun` onto `Fn` so user `trait T for Fun` impls register under the
/// same key as `trait T for Fn` (and as the runtime dispatch name).
#[test]
fn canonicalize_type_name_collapses_fun_to_fn() {
    let src = include_str!("../src/types/canonical.rs");
    assert!(
        src.contains(r#"if name_str.as_str() == "Fun""#),
        "expected `canonicalize_type_name` to collapse `Fun → Fn`; \
         the round 71 follow-up alias lock is broken"
    );
}
