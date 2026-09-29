//! Round 94 (BROKEN): module names took precedence over local bindings
//! in dotted-name resolution.
//!
//! When an identifier was BOTH a local binding (fn param, lambda param,
//! let, pattern binder) AND an imported module name, `x.member`
//! resolved to the MODULE, hijacking field access on the local:
//!
//!   1. USER PARAM: `import other` + `fn f(other: P) { other.year }`
//!      → `error[type]: unknown function 'year' on module 'other'`.
//!   2. BUILTIN MODULE: `import list` + `fn f(list: P) { list.map }`
//!      (P has field `map: Int`) → resolved to builtin `list.map`'s fn
//!      type → bogus type mismatch.
//!   3. SYNTHESIZED DERIVES: builtin auto-derive bodies use a parameter
//!      literally named `other` (`fn compare(self, other)` /
//!      `fn equal(self, other)` in `builtin_trait_decls`), so importing
//!      ANY user module named `other` broke `==` / `<` on EVERY record
//!      and on builtin Date comparisons.
//!
//! Required semantics: lexical shadowing. A value binding shadows a
//! same-named imported module within its scope; at top level with no
//! binding in scope, `other.double(21)` stays a module call.
//!
//! Fixes under test:
//!   * typechecker: `TypeChecker::value_binding_shadows_module` gates
//!     the FieldAccess module-resolution block, the Call arm's
//!     module-call detection and `callee_module_is_in_scope`
//!     (src/typechecker/inference.rs);
//!   * qualified type paths: a shadowed qualifier in record literals /
//!     variant patterns errors clearly instead of silently picking the
//!     module (`lookup_qualified_record` /
//!     `resolve_pattern_ctor_qualifier`);
//!   * compiler: top-level `let` binders are tracked in
//!     `top_level_value_globals` so dotted access on them compiles as
//!     field access, matching the typechecker (locals/upvalues were
//!     already resolved first by codegen);
//!   * effects walker: a shadowed/rebound dotted-callee base no longer
//!     charges the same-named MODULE fn's declared effects
//!     (src/typechecker/effects_infer.rs).

use std::path::PathBuf;
use std::process::Command;

// ── Helpers ─────────────────────────────────────────────────────────

fn rand_u64() -> u64 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Create a fresh tempdir holding the supplied module files plus a
/// `main.silt` containing `main_source`. Returns the dir path.
fn setup_dir(label: &str, files: &[(&str, &str)], main_source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "silt_r94_shadow_{label}_{}_{}",
        std::process::id(),
        rand_u64()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    for (name, content) in files {
        std::fs::write(dir.join(name), content).expect("write module");
    }
    std::fs::write(dir.join("main.silt"), main_source).expect("write main");
    dir
}

/// Run `silt <args> main.silt` in `dir`; return (stdout, stderr, ok).
fn silt_in_dir(dir: &PathBuf, args: &[&str]) -> (String, String, bool) {
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .args(args)
        .arg(dir.join("main.silt"))
        .output()
        .expect("spawn silt");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Run a program with module files and assert success; return stdout.
fn run_ok(label: &str, files: &[(&str, &str)], main_source: &str) -> String {
    let dir = setup_dir(label, files, main_source);
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["run"]);
    assert!(
        ok,
        "silt run should succeed for {label}; stdout={stdout}, stderr={stderr}"
    );
    stdout
}

/// Run a program and assert it FAILS; return combined output.
fn run_err(label: &str, files: &[(&str, &str)], main_source: &str) -> String {
    let dir = setup_dir(label, files, main_source);
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["run"]);
    assert!(
        !ok,
        "silt run should fail for {label}; stdout={stdout}, stderr={stderr}"
    );
    format!("{stdout}\n{stderr}")
}

/// The user module used by most tests — the name `other` collides with
/// the synthesized auto-derive parameter name.
const OTHER_MODULE: (&str, &str) = ("other.silt", "pub fn double(x) { x * 2 }\n");

fn assert_lines(label: &str, out: &str, expected: &[&str]) {
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines, expected,
        "unexpected output for {label}; got {out:?}"
    );
}

// ── Repro 1: user param shadows user module ─────────────────────────

/// `fn f(other: P) -> Int { other.year }` with `import other` in scope.
/// Before the fix: `error[type]: unknown function 'year' on module
/// 'other'`. After: field access on the param.
#[test]
fn param_shadows_user_module_field_access() {
    let out = run_ok(
        "param_user",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn f(other: P) -> Int { other.year }
fn main() { println(f(P { year: 5 })) }
"#,
    );
    assert_lines("param_user", &out, &["5"]);
}

// ── Repro 2: param shadows BUILTIN module ───────────────────────────

/// `fn f(list: P) -> Int { list.map }` where P has field `map: Int` and
/// `import list` is in scope. Before the fix the field access resolved
/// to builtin `list.map`'s fn type (type mismatch). After: the local
/// wins — AND the real `list.map` keeps working elsewhere in the same
/// program.
#[test]
fn param_shadows_builtin_module_field_access_and_module_still_works() {
    let out = run_ok(
        "param_builtin",
        &[],
        r#"
import list
type P { map: Int }
fn f(list: P) -> Int { list.map }
fn main() {
  println(f(P { map: 7 }))
  println(list.map([1, 2, 3], fn(x) { x * 10 }))
}
"#,
    );
    assert_lines("param_builtin", &out, &["7", "[10, 20, 30]"]);
}

// ── Repro 3: synthesized auto-derive bodies (param named `other`) ───

/// Importing a module named `other` must not break the synthesized
/// `==` / `<` / `.compare()` / `.hash()` bodies, whose comparison
/// parameter is literally named `other`. RUNTIME assertions: values
/// must actually compare correctly.
#[test]
fn derives_survive_import_of_module_named_other() {
    let out = run_ok(
        "derives",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn main() {
  let a = P { year: 5 }
  let b = P { year: 6 }
  println(a == b)
  println(a == P { year: 5 })
  println(a < b)
  println(b < a)
  println(a.compare(b))
  println(a.hash() == P { year: 5 }.hash())
}
"#,
    );
    assert_lines(
        "derives",
        &out,
        &["false", "true", "true", "false", "-1", "true"],
    );
}

/// Builtin Date comparison also goes through a synthesized compare
/// body with a param named `other` — `import other` alone (no user
/// records at all) used to break it at typecheck time.
#[test]
fn builtin_date_comparison_survives_import_of_module_named_other() {
    let out = run_ok(
        "date_compare",
        &[OTHER_MODULE],
        r#"
import other
import time
fn main() {
  let a = time.date(2020, 1, 1)
  let b = time.date(2021, 1, 1)
  println(a < b)
  println(a == b)
  println(other.double(21))
}
"#,
    );
    assert_lines("date_compare", &out, &["true", "false", "42"]);
}

// ── Shadow then leave scope ─────────────────────────────────────────

/// The same program uses `other` as a param (field access) inside `f`
/// and as a module (call) at top level — both must work.
#[test]
fn shadow_inside_fn_module_call_outside() {
    let out = run_ok(
        "leave_scope",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn f(other: P) -> Int { other.year }
fn main() {
  println(f(P { year: 5 }))
  println(other.double(2))
}
"#,
    );
    assert_lines("leave_scope", &out, &["5", "4"]);
}

// ── Shadowing via let (fn body) ─────────────────────────────────────

#[test]
fn fn_body_let_shadows_module() {
    let out = run_ok(
        "body_let",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn main() {
  println(other.double(3))
  let other = P { year: 11 }
  println(other.year)
}
"#,
    );
    assert_lines("body_let", &out, &["6", "11"]);
}

// ── Shadowing via TOP-LEVEL let ─────────────────────────────────────

/// Top-level lets are compiled as VM globals, not locals — the
/// compiler must agree with the typechecker that the value binding
/// shadows the module (codegen: `top_level_value_globals`).
#[test]
fn top_level_let_shadows_module() {
    let out = run_ok(
        "top_let",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
let other = P { year: 9 }
fn main() { println(other.year) }
"#,
    );
    assert_lines("top_let", &out, &["9"]);
}

/// Pre-existing adjacent gap, fixed by the same compiler change: field
/// access on a top-level let used to emit `GetGlobal("name.field")`
/// and die at runtime with `undefined global` — even with no module
/// collision at all.
#[test]
fn top_level_let_field_access_without_any_module() {
    let out = run_ok(
        "top_let_plain",
        &[],
        r#"
type P { year: Int }
let other = P { year: 9 }
fn main() { println(other.year) }
"#,
    );
    assert_lines("top_let_plain", &out, &["9"]);
}

// ── Shadowing via match-arm pattern binder ──────────────────────────

#[test]
fn match_arm_binder_shadows_module() {
    let out = run_ok(
        "match_binder",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn main() {
  let p = P { year: 21 }
  let got = match p {
    other -> other.year
  }
  println(got)
  println(other.double(got))
}
"#,
    );
    assert_lines("match_binder", &out, &["21", "42"]);
}

// ── Shadowing via lambda param ──────────────────────────────────────

#[test]
fn lambda_param_shadows_module() {
    let out = run_ok(
        "lambda_param",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn main() {
  let get_year = fn(other) { other.year }
  println(get_year(P { year: 33 }))
  println(other.double(4))
}
"#,
    );
    assert_lines("lambda_param", &out, &["33", "8"]);
}

// ── Local named like a module used as a CALL ────────────────────────

/// `fn f(other: Fn(Int) -> Int) { other(3) }` with `import other` —
/// the local fn value wins.
#[test]
fn fn_typed_param_named_like_module_is_callable() {
    let out = run_ok(
        "fn_param_call",
        &[OTHER_MODULE],
        r#"
import other
fn f(other: Fn(Int) -> Int) -> Int { other(3) }
fn main() {
  println(f(fn(x) { x + 100 }))
  println(other.double(3))
}
"#,
    );
    assert_lines("fn_param_call", &out, &["103", "6"]);
}

/// A record field that is itself a function, named like a module fn,
/// called through a shadowing param: must dispatch to the FIELD.
#[test]
fn fn_valued_field_named_like_module_fn_dispatches_to_field() {
    let out = run_ok(
        "fn_field_call",
        &[OTHER_MODULE],
        r#"
import other
type P { double: Fn(Int) -> Int }
fn f(other: P) -> Int { other.double(5) }
fn main() {
  println(f(P { double: fn(x) { x + 1 } }))
  println(other.double(5))
}
"#,
    );
    // The field triples... no: field adds 1 → 6; the module doubles → 10.
    assert_lines("fn_field_call", &out, &["6", "10"]);
}

// ── Diagnostics on shadowed names ───────────────────────────────────

/// A bad field access on a shadowed name must produce a FIELD
/// diagnostic for the local's type — not "unknown function on module"
/// and no module-fn "did you mean" noise.
#[test]
fn bad_field_on_shadowed_name_reports_record_field_error() {
    let out = run_err(
        "bad_field",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn f(other: P) -> Int { other.nope }
fn main() { println(f(P { year: 5 })) }
"#,
    );
    assert!(
        !out.contains("on module"),
        "shadowed access must not produce a module diagnostic; got {out}"
    );
    assert!(
        out.contains("no field 'nope'") || out.contains("'nope'"),
        "expected a record-field diagnostic mentioning 'nope'; got {out}"
    );
}

// ── Qualified type paths: shadowed qualifier errors clearly ─────────

/// `util.Pt { .. }` with a local `util` in scope: record-literal
/// qualifiers are type positions, so silently picking the module would
/// be misleading. The conservative behavior is a clear error.
#[test]
fn qualified_record_literal_with_shadowed_qualifier_errors_clearly() {
    let out = run_err(
        "qual_record",
        &[("util.silt", "pub type Pt { x: Int }\n")],
        r#"
import util
fn f(util: Int) -> Int {
  let p = util.Pt { x: 1 }
  p.x
}
fn main() { println(f(0)) }
"#,
    );
    assert!(
        out.contains("a local binding named 'util' shadows module 'util'"),
        "expected the shadowed-qualifier diagnostic; got {out}"
    );
}

/// Same conservative error for a qualified VARIANT pattern whose
/// qualifier is shadowed.
#[test]
fn qualified_variant_pattern_with_shadowed_qualifier_errors_clearly() {
    let out = run_err(
        "qual_variant_pattern",
        &[(
            "shapes.silt",
            "pub type Shape { Circle(Int), Square(Int) }\n",
        )],
        r#"
import shapes
fn f(shapes: Int) -> Int {
  match 1 {
    shapes.Circle(r) -> r,
    _ -> 0
  }
}
fn main() { println(f(0)) }
"#,
    );
    assert!(
        out.contains("a local binding named 'shapes' shadows module 'shapes'"),
        "expected the shadowed-qualifier pattern diagnostic; got {out}"
    );
}

/// Sanity: with NO shadowing binding in scope, qualified paths keep
/// working (the round-94 qualified-paths feature must stay intact).
#[test]
fn qualified_paths_still_work_without_shadowing() {
    let out = run_ok(
        "qual_ok",
        &[(
            "shapes.silt",
            "pub type Shape { Circle(Int), Square(Int) }\npub type Pt { x: Int }\n",
        )],
        r#"
import shapes
fn main() {
  let p = shapes.Pt { x: 7 }
  println(p.x)
  match shapes.Circle(3) {
    shapes.Circle(r) -> println(r),
    _ -> println(0)
  }
}
"#,
    );
    assert_lines("qual_ok", &out, &["7", "3"]);
}

// ── Effects ─────────────────────────────────────────────────────────

/// A pure fn whose param shadows the builtin `io` module: pure FIELD
/// access through the shadowed name must not charge io effects under
/// --strict-effects. Before the fix this errored at typecheck time
/// ("unknown function 'size' on module 'io'").
#[test]
fn strict_effects_shadowed_io_param_field_access_is_pure() {
    let dir = setup_dir(
        "strict_pure",
        &[],
        r#"
import io
type Wrapper { size: Int }
fn size_of(io: Wrapper) -> Int { io.size }
fn main() !{io} { println(size_of(Wrapper { size: 3 })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["run", "--strict-effects"]);
    assert!(
        ok,
        "pure field access through a shadowed module name must pass --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    assert_eq!(stdout.trim(), "3");
}

/// Vice versa: when NOT shadowed, a real module call's effects must
/// still be charged under --strict-effects.
#[test]
fn strict_effects_unshadowed_module_call_still_charged() {
    let dir = setup_dir(
        "strict_charged",
        &[],
        r#"
import io
fn bad() -> Result(String, IoError) { io.read_line() }
fn main() !{io} {
  match bad() {
    Ok(s) -> println(s),
    Err(_) -> println("err")
  }
}
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["check", "--strict-effects"]);
    assert!(
        !ok,
        "unshadowed io call in an unannotated fn must fail --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("io"),
        "diagnostic should mention the io effect; got {combined}"
    );
}

/// Effects walker soundness lock: a dotted CALL through a SHADOWING
/// param must NOT silently adopt the same-named module fn's (pure)
/// declared effects. Phase A can't see through a fn-typed FIELD, so
/// the conservative answer is TOP — the unannotated fn fails
/// --strict-effects instead of letting a potentially effectful field
/// fn slip through as "pure like the module fn".
#[test]
fn strict_effects_shadowed_dotted_call_is_conservative_not_module_pure() {
    let module = ("other.silt", "pub fn double(x: Int) -> Int !{} { x * 2 }\n");
    // Unshadowed baseline: calling the pure-annotated module fn from an
    // unannotated fn passes --strict-effects (module effects honored).
    let dir_ok = setup_dir(
        "strict_unshadowed_pure",
        &[module],
        r#"
import other
fn fine() -> Int { other.double(3) }
fn main() !{io} { println(fine()) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir_ok, &["check", "--strict-effects"]);
    assert!(
        ok,
        "unshadowed pure module call must pass --strict-effects; stdout={stdout}, stderr={stderr}"
    );
    // Shadowed: same dotted spelling, but `other` is a param whose
    // field is a fn value — the module's `!{}` must not transfer.
    let dir_bad = setup_dir(
        "strict_shadowed_conservative",
        &[module],
        r#"
import other
type P { double: Fn(Int) -> Int }
fn sneaky(other: P) -> Int { other.double(3) }
fn main() !{io} { println(sneaky(P { double: fn(x) { x + 1 } })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir_bad, &["check", "--strict-effects"]);
    assert!(
        !ok,
        "shadowed dotted call must be charged conservatively (TOP), not the module fn's !{{}}; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("'sneaky'"),
        "the conservative charge should land on 'sneaky'; got {combined}"
    );
}

// ── silt check: shadowed import doesn't crash / stays well-behaved ──

/// `silt check` on a program where the import is shadowed everywhere
/// it appears must succeed without crashing (the import is still
/// considered per existing usage rules — we only lock "no crash, no
/// bogus shadow-related error" here).
#[test]
fn check_with_everywhere_shadowed_import_is_clean() {
    let dir = setup_dir(
        "check_shadowed",
        &[OTHER_MODULE],
        r#"
import other
type P { year: Int }
fn f(other: P) -> Int { other.year }
fn main() { println(f(P { year: 5 })) }
"#,
    );
    let (stdout, stderr, ok) = silt_in_dir(&dir, &["check"]);
    assert!(
        ok,
        "silt check should pass; stdout={stdout}, stderr={stderr}"
    );
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("on module 'other'"),
        "no module-member diagnostic for shadowed access; got {combined}"
    );
}

// ── Module named `self` (probe lock) ────────────────────────────────

/// User modules named `self` are importable and callable at top level
/// (no builtin-collision validation reserves the name). Locked here so
/// any future change to that policy is a conscious one.
#[test]
fn module_named_self_is_callable_at_top_level() {
    let out = run_ok(
        "self_module",
        &[("self.silt", "pub fn me() { 42 }\n")],
        r#"
import self
fn main() { println(self.me()) }
"#,
    );
    assert_lines("self_module", &out, &["42"]);
}
