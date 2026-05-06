//! Round 74 Fix #1 lock: user-defined `trait T for ExtFloat { ... }`
//! impls must compile and dispatch.
//!
//! Pre-fix bug (`src/typechecker/mod.rs::type_from_name` at line ~4904):
//!   The bare-target arm of `register_trait_impl` calls
//!   `Self::type_from_name(target_type)` to build the impl's
//!   self_type. The pre-fix match listed `Int / Float / Bool / String`
//!   only — `ExtFloat` fell through to
//!   `_ => Type::Generic("ExtFloat", vec![])`, which never unifies
//!   with `Type::ExtFloat` produced by `Float / Float` or
//!   `math.sqrt(...)`. The user-impl path therefore typechecked the
//!   impl body once but every call site then surfaced
//!   "type mismatch: expected ExtFloat, got ExtFloat".
//!
//! Round 71 fixed only the auto-derived path (`register_auto_derived_impls_for`
//! at mod.rs:6764 includes `"ExtFloat"`). The user-impl path stayed
//! broken until round 74.
//!
//! Post-fix: `type_from_name` includes `"ExtFloat" => Type::ExtFloat`
//! (and `"Unit"|"()" => Type::Unit` for finding 4) so a user
//! `trait Show for ExtFloat { ... }` impl receives the canonical
//! `Type::ExtFloat` self_type and unifies with concrete ExtFloat
//! receivers.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round74_extfloat_{label}.silt"));
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

/// End-to-end repro from the audit finding: a user-declared
/// `trait Show for ExtFloat { fn show(self) -> String = "x" }` must
/// dispatch on a `Type::ExtFloat` receiver and print `"x"`.
#[test]
fn user_trait_for_extfloat_dispatches_at_runtime() {
    let out = run_silt_ok(
        "user_show_for_extfloat",
        r#"
trait Show { fn show(self) -> String }
trait Show for ExtFloat { fn show(self) -> String = "x" }
fn main() {
  let x: ExtFloat = 1.0/1.0
  println(x.show())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "x",
        "user-defined Show for ExtFloat should print 'x' (round 74 Fix #1); got {out:?}"
    );
}
