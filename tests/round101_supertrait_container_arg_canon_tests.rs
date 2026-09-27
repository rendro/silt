//! Round 101 BROKEN regression lock: `resolve_supertrait_arg`
//! (src/typechecker/inference.rs) built NON-CANONICAL type shapes for
//! builtin containers and missed ExtFloat/Unit entirely, spuriously
//! rejecting valid supertrait-arg impls with a self-contradictory
//! error.
//!
//! Pre-fix, the Named arm only mapped Int/Float/Bool/String to their
//! `Type::…` primitives — `ExtFloat` and `Unit`/`()` fell through to
//! `Type::Generic(sym, [])` — and the Generic arm ALWAYS built
//! `Type::Generic`, so a supertrait arg like `List(b)` resolved to
//! `Generic("List", [Int])` while the impl side (`resolve_type_expr`)
//! built the canonical `Type::List(Int)`. `canonicalize()` does not
//! collapse `Generic("List", …)` onto `Type::List`, so
//! `trait_arg_compatible_canon` had no cross-arm and the supertrait
//! obligation check fired with IDENTICAL Display strings on both
//! sides:
//!
//!   error[type]: impl Holds(Int) for Bag requires
//!     impl Carry(List(Int)) for Bag, but found
//!     impl Carry(List(Int)) for Bag
//!
//! (For Unit the mismatch showed as `requires impl Carry(Unit) …
//! but found impl Carry(()) …`.) The same wrong shape flowed into
//! `trait_arg_bindings` (where-bound supertrait expansion), so the
//! dispatch path through a `where t: Holds(Int)` bound was equally
//! broken. Verified failing on the HEAD binary at 76987f8 for
//! List(b), Map(String, b), ExtFloat, and Unit supertrait args; the
//! bare-param control `Carry(b)` passed.
//!
//! Every positive test here runs BOTH `silt check` (exit 0) and
//! `silt run` (executes the supertrait-dispatched methods and asserts
//! stdout), covering the full parser → typechecker → compiler → VM
//! pipeline. The negative control locks that the fix did not degrade
//! the round-60 B1 supertrait-arg compatibility check into
//! always-true: a genuinely mismatched container arg must still be
//! rejected, now with two DIFFERENT type strings in the message.

use std::process::Command;

fn silt_cmd(label: &str, sub: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!(
        "silt_round101_supertrait_canon_{label}_{}.silt",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg(sub)
        .arg(&tmp)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

/// `silt check` must exit 0, then `silt run` must exit 0 and print
/// exactly `expected_stdout`.
fn assert_checks_and_runs(label: &str, src: &str, expected_stdout: &str) {
    let (_, check_err, check_ok) = silt_cmd(label, "check", src);
    assert!(
        check_ok,
        "silt check should accept the {label} supertrait-arg program, \
         got stderr:\n{check_err}"
    );
    let (run_out, run_err, run_ok) = silt_cmd(label, "run", src);
    assert!(
        run_ok,
        "silt run should execute the {label} supertrait-arg program, \
         got stderr:\n{run_err}"
    );
    assert_eq!(
        run_out, expected_stdout,
        "unexpected stdout for {label} (stderr:\n{run_err})"
    );
}

// ── Positive: builtin-container supertrait args must be accepted ────

/// The verified repro: `Holds(b): Carry(List(b))` with
/// `impl Carry(List(Int)) for Bag` + `impl Holds(Int) for Bag`.
/// Also routes `carry()` through a `where t: Holds(Int)` bound so the
/// `trait_arg_bindings` expansion path (inference.rs) is exercised at
/// EXECUTION time, not just the impl-registration obligation check.
#[test]
fn list_supertrait_arg_checks_and_runs() {
    let src = r#"
type Bag { n: Int }
trait Carry(c) { fn carry(self) -> c }
trait Holds(b): Carry(List(b)) { fn hold(self) -> b }
trait Carry(List(Int)) for Bag { fn carry(self) -> List(Int) { [1, 2] } }
trait Holds(Int) for Bag { fn hold(self) -> Int { 1 } }
fn first_carried(x: t) -> List(Int) where t: Holds(Int) { x.carry() }
fn main() {
  let b = Bag { n: 1 }
  println("{b.hold()}")
  println("{first_carried(b)}")
}
"#;
    assert_checks_and_runs("list", src, "1\n[1, 2]\n");
}

/// Two-arg builtin container head: `Map(String, b)`.
#[test]
fn map_supertrait_arg_checks_and_runs() {
    let src = r#"
type Bag { n: Int }
trait Carry(c) { fn carry(self) -> c }
trait Holds(b): Carry(Map(String, b)) { fn hold(self) -> b }
trait Carry(Map(String, Int)) for Bag { fn carry(self) -> Map(String, Int) { #{"a": 1} } }
trait Holds(Int) for Bag { fn hold(self) -> Int { 1 } }
fn main() {
  let b = Bag { n: 1 }
  println("{b.hold()}")
  println("{b.carry()}")
}
"#;
    assert_checks_and_runs("map", src, "1\n#{\"a\": 1}\n");
}

/// Named-arm gap: a concrete `ExtFloat` supertrait arg fell through
/// to `Generic("ExtFloat", [])` and never matched the impl side's
/// `Type::ExtFloat`. (`3.0 / 2.0` is the canonical ExtFloat producer.)
#[test]
fn extfloat_supertrait_arg_checks_and_runs() {
    let src = r#"
type Bag { n: Int }
trait Carry(c) { fn carry(self) -> c }
trait Holds(b): Carry(ExtFloat) { fn hold(self) -> b }
trait Carry(ExtFloat) for Bag { fn carry(self) -> ExtFloat { 3.0 / 2.0 } }
trait Holds(Int) for Bag { fn hold(self) -> Int { 1 } }
fn main() {
  let b = Bag { n: 1 }
  println("{b.hold()}")
  println("{b.carry()}")
}
"#;
    assert_checks_and_runs("extfloat", src, "1\n1.5\n");
}

/// Named-arm gap: `Unit` fell through to `Generic("Unit", [])`; the
/// pre-fix error even displayed the two spellings asymmetrically
/// ("requires impl Carry(Unit) … but found impl Carry(()) …").
#[test]
fn unit_supertrait_arg_checks_and_runs() {
    let src = r#"
type Bag { n: Int }
trait Carry(c) { fn carry(self) -> c }
trait Holds(b): Carry(Unit) { fn hold(self) -> b }
trait Carry(Unit) for Bag { fn carry(self) -> Unit { () } }
trait Holds(Int) for Bag { fn hold(self) -> Int { 1 } }
fn main() { println("{(Bag { n: 1 }).hold()}") }
"#;
    assert_checks_and_runs("unit", src, "1\n");
}

// ── Negative control: genuine mismatches must STILL be rejected ─────

/// `Holds(Int)` requires `Carry(List(Int))`, but only
/// `Carry(List(String))` is implemented. The obligation must still
/// fire — proving the fix made the two sides COMPARABLE rather than
/// making `trait_arg_compatible` vacuously true — and the message must
/// now show two genuinely different type arguments.
#[test]
fn mismatched_list_supertrait_arg_still_rejected() {
    let src = r#"
type Bag { n: Int }
trait Carry(c) { fn carry(self) -> c }
trait Holds(b): Carry(List(b)) { fn hold(self) -> b }
trait Carry(List(String)) for Bag { fn carry(self) -> List(String) { ["x"] } }
trait Holds(Int) for Bag { fn hold(self) -> Int { 1 } }
fn main() { println("{(Bag { n: 1 }).hold()}") }
"#;
    let (_, stderr, ok) = silt_cmd("neg_list", "check", src);
    assert!(
        !ok,
        "silt check must reject impl Carry(List(String)) under \
         Holds(Int): Carry(List(b)), but it passed"
    );
    assert!(
        stderr.contains("requires impl Carry(List(Int)) for Bag")
            && stderr.contains("found impl Carry(List(String)) for Bag"),
        "expected the supertrait-obligation error naming List(Int) \
         (required) vs List(String) (found), got:\n{stderr}"
    );
}
