//! Round 74 Fix #4 lock: user-defined `trait T for Unit { ... }`
//! impls must compile and dispatch on `Type::Unit` receivers.
//!
//! Pre-fix bug (audit GAP-4, "symbol drift"):
//!   - Parser at `src/parser.rs:1745-1755` accepts `Named("Unit")` as
//!     the impl target (the Symbol "Unit").
//!   - FieldAccess dispatch at `src/typechecker/inference.rs:2576`
//!     keys Unit lookups under `intern("()")`.
//!   - Auto-derive registers under `"()"` (`mod.rs:6764`).
//!   - `head_symbol_of_canon(Type::Unit) = "Unit"` (`canonical.rs:524`).
//!   - `dispatch_name_for_value(Value::Unit) = "Unit"`
//!     (`canonical_name(Type::Unit)` returns `"Unit"`).
//!
//!   Result: a user `trait Greet for Unit { fn greet(...) }` registers
//!   under `("Greet", "Unit")` because the local `canonicalize_type_name`
//!   in `mod.rs:6213` did not collapse `"Unit"` onto `"()"`. The
//!   FieldAccess-arm dispatch keyed on `"()"` and the lookup missed
//!   — surface error: "unknown method 'greet' on type ()".
//!
//! Post-fix: the local `canonicalize_type_name` now collapses
//! `"Unit"` onto `"()"`, so user impls register under the same key
//! the dispatch path uses. Auto-derive's hard-coded `"()"` entry was
//! already aligned (`mod.rs:6764`).
//!
//! Note: this test will need to be updated whenever the broader
//! canonical-name convergence in `src/types/canonical.rs` lands —
//! that file is owned by a sibling agent and any further collapse
//! (e.g. `canonical_name(Type::Unit) → "()"`) will keep this lock
//! green by tightening, not loosening, the convergence.

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round74_unit_{label}.silt"));
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

fn run_silt_ok(label: &str, src: &str) -> String {
    let (stdout, stderr, ok) = run_silt_raw(label, src);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

/// End-to-end repro from the audit finding: a user `trait Greet for
/// Unit { fn greet(self) -> String = "hi" }` impl must dispatch on a
/// `()` receiver and print `"hi"`.
#[test]
fn user_trait_for_unit_dispatches_at_runtime() {
    let out = run_silt_ok(
        "user_greet_for_unit",
        r#"
trait Greet { fn greet(self) -> String }
trait Greet for Unit { fn greet(self) -> String = "hi" }
fn main() {
  let u = ()
  println(u.greet())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "hi",
        "user-defined Greet for Unit should print 'hi' (round 74 Fix #4); got {out:?}"
    );
}
