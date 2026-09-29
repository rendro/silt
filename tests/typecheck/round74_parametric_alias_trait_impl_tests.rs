//! Round 74 Fix #2 lock: parametric type alias as trait-impl target
//! must expand the alias's target through the user-supplied type-args.
//!
//! Pre-fix bug (`src/typechecker/mod.rs::register_trait_impl` at the
//! `target_type_args.is_empty() == false` branch, line ~5655-5723):
//!   The arity check at lines 5671-5680 consulted record_param_var_ids,
//!   self.enums, and the builtin container table — but NOT
//!   `type_alias_arity`. So `trait Show for Pair(a)` (with
//!   `type Pair(a) = (a, a)`) skipped the arity check and fell
//!   through to the `_ => Type::Generic("Pair", [tv])` arm at line
//!   5721. That self_type never unifies with a concrete `(Int, Int)`
//!   receiver — the call site reports "type mismatch: expected
//!   Pair(_), got (Int, Int)".
//!
//! Post-fix: the parametric branch looks up the alias and substitutes
//! `resolved_args` through `info.target` (mirroring the
//! non-parametric alias path at line ~5617 which does the same with
//! fresh tyvars).

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round74_palias_{label}.silt"));
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

/// End-to-end repro from the audit finding: a `trait Show for Pair(a)`
/// impl on a parametric tuple alias must expand through the alias and
/// unify with the concrete receiver `(1, 2)` (typed `(Int, Int)`).
#[test]
fn user_trait_for_parametric_alias_dispatches_at_runtime() {
    let out = run_silt_ok(
        "user_show_for_pair_alias",
        r#"
type Pair(a) = (a, a)
trait Show { fn show(self) -> String }
trait Show for Pair(a) { fn show(self) -> String = "p" }
fn main() {
  let p: Pair(Int) = (1, 2)
  println(p.show())
}
"#,
    );
    assert_eq!(
        out.trim(),
        "p",
        "user-defined Show for Pair(a) (alias = (a,a)) should print 'p' \
         (round 74 Fix #2); got {out:?}"
    );
}
