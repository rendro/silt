//! Round 103 regression lock: Tuple-/Fn-shaped alias impls must NOT
//! bypass the round-102 where-bound self-type-args check.
//!
//! Round 102 (`impl_self_types` + the positional-args comparison in
//! `verify_trait_obligation`) closed the head-key-only hole for alias
//! impls like `type Bytes2 = List(Int)`. But `type_args_of`
//! (src/typechecker/mod.rs) had no `Type::Tuple` / `Type::Fun` arms, so
//! for a tuple-shaped alias (`type P2 = (Int, Int)`) or a fn-shaped one
//! (`type IntOp = Fn(Int) -> Int`) BOTH the obligated type's args and
//! the impl self-type's args came back empty: the empty-vs-empty zip
//! never mismatched and ANY tuple / any function satisfied the bound.
//! Verified pre-fix: `sum_it(("a", "b"))` against the `(Int, Int)`-only
//! impl passed `silt check` (exit 0) and RAN, printing "ab" — a String
//! escaping a fn declared `-> Int`. Direct receiver dispatch
//! (`("a","b").total()`) correctly rejected, so only the where-bound
//! path was holed.
//!
//! The fix adds `Type::Tuple(elems) => elems` and
//! `Type::Fun(params, ret) => params ++ [ret]` arms to `type_args_of`.
//! Differing arities hit the existing equal-length conservative-skip
//! guard, so the bare `trait T for Tuple` wildcard impl
//! (`Generic("Tuple", [])`, zero args) keeps matching every tuple
//! (positive control below; also locked by
//! `bare_builtin_container_impl_target_dispatch_tests`). The round-102
//! List/Map locks live in
//! `round102_alias_impl_self_type_args_where_bound_tests` and must stay
//! green alongside this file.

use std::process::Command;

use silt::typechecker;
use silt::types::Severity;

// ── Helpers ─────────────────────────────────────────────────────────

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Run a silt source program via the `silt run` subprocess and return
/// (stdout, stderr, success). Mirrors the helper in
/// `tests/round102_alias_impl_self_type_args_where_bound_tests.rs`.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round103_tuple_fn_alias_{label}.silt"));
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

const P2_PRELUDE: &str = r#"
type P2 = (Int, Int)
trait Total { fn total(self) -> Int }
trait Total for P2 { fn total(self) -> Int = self.0 + self.1 }
fn sum_it(x: a) -> Int where a: Total { x.total() }
"#;

// ── BROKEN: tuple alias impl satisfied any tuple ────────────────────

/// The audit repro: `(String, String)` must NOT satisfy `where a: Total`
/// when the only `Total` impl is the alias-expanded `(Int, Int)`.
///
/// Pre-fix this program passed `silt check` (exit 0) and `silt run`
/// printed "ab": `+` concatenated Strings inside the Int-only impl body
/// and the String escaped a fn declared `-> Int`.
#[test]
fn tuple_alias_impl_where_bound_rejects_mismatched_elements() {
    let src = format!(
        r#"{P2_PRELUDE}
fn main() {{ println(sum_it(("a", "b"))) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Total'")),
        "(String, String) obligated against the (Int, Int)-only impl MUST \
         reject at check time; got: {errs:?}"
    );
    // Directional message: name the one impl that exists.
    assert!(
        errs.iter()
            .any(|e| e.contains("(String, String)") && e.contains("(Int, Int)")),
        "diagnostic should cite both the obligated type (String, String) \
         and the impl's (Int, Int); got: {errs:?}"
    );

    // Belt-and-braces: `silt run` must not succeed either.
    let (stdout, _stderr, ok) = run_silt_raw("reject_string_pair", &src);
    assert!(
        !ok,
        "silt run must fail for (String, String) against the (Int, Int)-only \
         impl; it printed: {stdout:?}"
    );
}

// ── Positive control: the matching instantiation still RUNS ─────────

/// `sum_it((1, 2))` through the same where-bound fn must keep
/// typechecking AND executing (prints 3). Guards against the fix
/// over-rejecting the legitimate instantiation.
#[test]
fn tuple_alias_impl_where_bound_runs_matching_elements() {
    let src = format!(
        r#"{P2_PRELUDE}
fn main() {{ println(sum_it((1, 2))) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "(Int, Int) satisfies the alias-expanded (Int, Int) impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_int_pair", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "3", "sum of (1, 2) through the where-bound");
}

// ── BROKEN: fn-shaped alias impl satisfied any function ─────────────

const INTOP_PRELUDE: &str = r#"
type IntOp = Fn(Int) -> Int
trait Apply1 { fn app(self) -> Int }
trait Apply1 for IntOp { fn app(self) -> Int = self(41) + 1 }
fn go(x: a) -> Int where a: Apply1 { x.app() }
"#;

/// A `Fn(String) -> String` lambda must NOT satisfy `where a: Apply1`
/// when the only impl is the alias-expanded `Fn(Int) -> Int`.
///
/// Pre-fix this passed `silt check` and crashed at runtime with
/// "cannot apply '+' to Int and String" inside the impl body.
#[test]
fn fn_alias_impl_where_bound_rejects_mismatched_shape() {
    let src = format!(
        r#"{INTOP_PRELUDE}
fn main() {{ println(go({{ s -> s + "!" }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Apply1'")),
        "Fn(String) -> String obligated against the Fn(Int) -> Int-only \
         impl MUST reject at check time; got: {errs:?}"
    );
    // Directional message: name the one impl that exists.
    assert!(
        errs.iter()
            .any(|e| e.contains("Fn(String) -> String") && e.contains("Fn(Int) -> Int")),
        "diagnostic should cite both the obligated fn shape and the \
         impl's Fn(Int) -> Int; got: {errs:?}"
    );
    let (stdout, _stderr, ok) = run_silt_raw("reject_string_op", &src);
    assert!(
        !ok,
        "silt run must fail for the String lambda against the \
         Fn(Int) -> Int-only impl; it printed: {stdout:?}"
    );
}

/// ... while an `Fn(Int) -> Int` lambda matches and RUNS end-to-end
/// (self(41) evaluates the lambda: (41 + 1) + 1 == 43).
#[test]
fn fn_alias_impl_where_bound_runs_matching_shape() {
    let src = format!(
        r#"{INTOP_PRELUDE}
fn main() {{ println(go({{ x -> x + 1 }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "Fn(Int) -> Int satisfies the alias-expanded impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_int_op", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "43");
}

// ── Positive control: bare `for Tuple` wildcard keeps matching ──────

/// The bare `trait Tag for Tuple` impl registers self_type
/// `Generic("Tuple", [])` — zero positional args. Against any concrete
/// tuple the arg lists have different lengths, which must hit the
/// equal-length conservative-skip guard (NOT reject), so the wildcard
/// impl keeps dispatching for every tuple shape through a where-bound.
#[test]
fn bare_tuple_impl_where_bound_still_matches_any_tuple() {
    let src = r#"
trait Tag { fn tag(self) -> String }
trait Tag for Tuple { fn tag(self) -> String = "tuple" }
fn tag_it(x: a) -> String where a: Tag { x.tag() }
fn main() { println(tag_it((1, "x", true))) }
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "bare `for Tuple` impl must satisfy any tuple shape; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("bare_tuple_wildcard", src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "tuple");
}
