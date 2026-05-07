//! Round 77: lock test for the `dispatch_name_for_value` BROKEN bug
//! where `Value::BuiltinFn(_)` and `Value::VariantConstructor(..)` were
//! still returning their own variant tags (`"BuiltinFn"` /
//! `"VariantConstructor"`) instead of the canonical function-type
//! dispatch name `"Fn"`.
//!
//! Pre-fix state (audit TYPE-1 BROKEN, round 77):
//!
//!   The typechecker types `BuiltinFn` and `VariantConstructor` values
//!   as `Type::Fun(..)` (they are function-shaped). `canonical_name
//!   (Type::Fun) == "Fn"` / `head_symbol_of_canon(Type::Fun) == "Fn"`,
//!   so a user `trait T for Fn { ... }` impl registers under the
//!   `("T", "Fn")` key. The round-71 follow-up unified
//!   `Value::VmClosure(_) => "Fn"` but the sibling
//!   `Value::BuiltinFn(_) => "BuiltinFn"` /
//!   `Value::VariantConstructor(..) => "VariantConstructor"` arms were
//!   left behind. Result: binding a builtin (`let h = println`) or a
//!   variant constructor (`let h = May`) and calling `h.describe()`
//!   produced `error[runtime]: no method 'describe' for type
//!   'BuiltinFn'` (or `'VariantConstructor'`) even though the
//!   `for Fn` impl was registered.
//!
//! Post-fix state (round 77):
//!
//!   All three function-shaped `Value` arms in `dispatch_name_for_value`
//!   produce `"Fn"`, matching `canonical_name(Type::Fun)` and
//!   `head_symbol_of_canon(Type::Fun)`. The end-to-end runtime tests
//!   below assert dispatch succeeds for both builtins and variant
//!   constructors. The source-grep negative lock asserts the pre-fix
//!   literals do not return.
//!
//! This file supersedes the typecheck-only weak gate at
//! `tests/round71_followup_fn_canonical_name_tests.rs` for the
//! sibling arms — round 71's existing `VmClosure` runtime test
//! (`user_trait_for_fn_dispatches_correctly_at_runtime`) covers the
//! third arm.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round77_{label}.silt"));
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

/// `trait MyTrait for Fn` must dispatch at runtime when the receiver is
/// a `Value::BuiltinFn` (e.g. `println` bound via `let h = println`).
/// Pre-fix: the impl registered under `("MyTrait", "Fn")` while the VM
/// dispatched under `"BuiltinFn"`, so `<Type>.<method>` lookup missed
/// and the user saw `no method 'describe' for type 'BuiltinFn'`.
#[test]
fn for_fn_dispatches_on_builtin_fn_value() {
    let out = run_silt_ok(
        "for_fn_builtin",
        r#"
trait MyTrait { fn describe(self) -> String }
trait MyTrait for Fn { fn describe(self) -> String = "[fn]" }
fn main() {
  let h = println
  println(h.describe())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "[fn]",
        "for Fn impl should dispatch on a BuiltinFn receiver; got {out:?}"
    );
}

/// Same `trait MyTrait for Fn` must dispatch when the receiver is a
/// `Value::VariantConstructor` (a bare constructor reference, not a
/// fully-applied variant). Pre-fix: the VM dispatched under
/// `"VariantConstructor"`, missing the `("MyTrait", "Fn")` key.
#[test]
fn for_fn_dispatches_on_variant_constructor_value() {
    let out = run_silt_ok(
        "for_fn_variant_ctor",
        r#"
trait MyTrait { fn describe(self) -> String }
trait MyTrait for Fn { fn describe(self) -> String = "[fn]" }
type Maybe(a) { May(a) }
fn main() {
  let h = May
  println(h.describe())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "[fn]",
        "for Fn impl should dispatch on a VariantConstructor receiver; got {out:?}"
    );
}

// ── Source-grep negative lock ───────────────────────────────────────

/// Pre-fix literals must not return. The audit found that
/// `dispatch_name_for_value` produced the variant tag instead of the
/// canonical name; this lock makes a future revert visible at test
/// time. Two separate substring assertions so the failure message
/// names the offending arm directly.
#[test]
fn canonical_dispatch_name_for_value_does_not_emit_old_variant_tags() {
    let src = include_str!("../src/types/canonical.rs");

    // Use the full assignment expressions as the search keys so
    // unrelated comment text mentioning the same string literals
    // (e.g. this very test's docstring rendered into a sibling
    // comment) does NOT false-positive. The bug shape is specifically
    // a `Some("<Tag>".to_string())` on the `BuiltinFn` /
    // `VariantConstructor` arms.
    assert!(
        !src.contains(r#"Value::BuiltinFn(_) => Some("BuiltinFn".to_string())"#),
        "pre-fix literal `Value::BuiltinFn(_) => Some(\"BuiltinFn\".to_string())` \
         must not return; canonical dispatch name for function-shaped \
         values is `\"Fn\"` (round 77 BROKEN fix lock)"
    );
    assert!(
        !src.contains(
            r#"Value::VariantConstructor(..) => Some("VariantConstructor".to_string())"#
        ),
        "pre-fix literal `Value::VariantConstructor(..) => \
         Some(\"VariantConstructor\".to_string())` must not return; \
         canonical dispatch name for function-shaped values is `\"Fn\"` \
         (round 77 BROKEN fix lock)"
    );

    // Positive companion: assert the post-fix literals are present so
    // a partial revert (back to `BuiltinFn` / `VariantConstructor`
    // tags) is caught even if the pre-fix shapes are reformatted.
    assert!(
        src.contains(r#"Value::BuiltinFn(_) => Some("Fn".to_string())"#),
        "expected `dispatch_name_for_value` BuiltinFn arm to produce `\"Fn\"`; \
         the round 77 BROKEN fix lock is broken"
    );
    assert!(
        src.contains(r#"Value::VariantConstructor(..) => Some("Fn".to_string())"#),
        "expected `dispatch_name_for_value` VariantConstructor arm to produce `\"Fn\"`; \
         the round 77 BROKEN fix lock is broken"
    );
}
