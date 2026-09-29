//! Round 74 Fix #4 lock (round-75 update): user-defined
//! `trait T for Unit { ... }` impls must compile and dispatch on
//! `Type::Unit` receivers.
//!
//! Pre-fix bug (audit GAP-4, "symbol drift"):
//!   - Parser accepts `Named("Unit")` as the impl target.
//!   - FieldAccess dispatch in `src/typechecker/inference.rs` keys
//!     Unit lookups (the round-74 fix initially used `intern("()")`,
//!     round 75 flipped to `intern("Unit")` — see below).
//!   - Auto-derive (round-74 used `"()"`; round-75 flipped to `"Unit"`).
//!   - `head_symbol_of_canon(Type::Unit) = "Unit"`.
//!   - `dispatch_name_for_value(Value::Unit) = "Unit"`.
//!
//! Round 74 originally collapsed `"Unit"` → `"()"` in the
//! typechecker's `canonicalize_type_name` so the FieldAccess and
//! auto-derive (`"()"`) sites converged on `"()"`. That made
//! typecheck pass, but the runtime path (compiler emits globals via
//! `canonicalize_type_name`; VM dispatches via
//! `dispatch_name_for_value(Value::Unit) = "Unit"`) still keyed on
//! `"Unit"`. Round 75 flipped both sides to the canonical direction
//! `"()"` → `"Unit"` and updated the FieldAccess + auto-derive sites
//! accordingly, so all of typechecker / compiler / VM converge on
//! the single canonical key `"Unit"`.

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
