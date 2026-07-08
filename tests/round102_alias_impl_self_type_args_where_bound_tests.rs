//! Round 102 regression lock: where-bound trait obligations must check
//! the matched impl's SELF-TYPE positional args, not just the head key.
//!
//! `verify_trait_obligation` (src/typechecker/mod.rs) used to check only
//! `trait_impl_set.contains(&(trait_name, head))`. An alias-expanded impl
//! with concrete self-type args — `type Bytes2 = List(Int)` plus
//! `trait Total for Bytes2` — registers under head `"List"` with
//! self_type `List(Int)` (the Phase-D alias path in `register_trait_impl`),
//! so the head-keyed membership check satisfied the where-bound for ANY
//! `List(T)`. `silt check` exited 0 on `sum_it(["a", "b"])` and the
//! Int-assuming method body then died at runtime with
//! "cannot apply '+' to Int and String" (or silently ran on the wrong
//! element type for element-agnostic bodies).
//!
//! The fix stores the impl's canonicalized self type in
//! `impl_self_types` keyed by `(trait_name, canonical_head)` and, after
//! the head-membership check passes, compares the obligated type's
//! positional args against the impl's with defer-on-Var logic. Generic
//! impls (`for List(a)`) store `Var` args and keep matching everything.
//!
//! Pre-fix: `alias_concrete_impl_where_bound_rejects_mismatched_elements`
//! and `parametric_alias_where_bound_rejects_mismatched_key_type` FAIL
//! (check passes, runtime crashes / wrong-type dispatch). The positive
//! controls exercise the EXECUTION path end-to-end so the fix cannot
//! degrade into rejecting valid instantiations.

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
/// `tests/trait_args_where_clause_runtime_tests.rs`.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round102_alias_self_type_{label}.silt"));
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

const BYTES2_PRELUDE: &str = r#"
import list
type Bytes2 = List(Int)
trait Total { fn total(self) -> Int }
trait Total for Bytes2 { fn total(self) -> Int = list.fold(self, 0, { acc, x -> acc + x }) }
fn sum_it(x: a) -> Int where a: Total { x.total() }
"#;

// ── BROKEN: alias-expanded concrete impl satisfied any List(T) ──────

/// The audit repro: `List(String)` must NOT satisfy `where a: Total`
/// when the only `Total` impl is the alias-expanded `List(Int)`.
///
/// Pre-fix this program passed `silt check` (exit 0) and crashed at
/// runtime inside `List.total` with "cannot apply '+' to Int and
/// String" — the two obligation paths disagreed (direct receiver
/// dispatch `["a","b"].total()` DID reject via method-entry unify).
#[test]
fn alias_concrete_impl_where_bound_rejects_mismatched_elements() {
    let src = format!(
        r#"{BYTES2_PRELUDE}
fn main() {{ println(sum_it(["a", "b"])) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Total'")),
        "List(String) obligated against the List(Int)-only impl MUST \
         reject at check time; got: {errs:?}"
    );
    // Directional message: name the one impl that exists.
    assert!(
        errs.iter()
            .any(|e| e.contains("List(String)") && e.contains("List(Int)")),
        "diagnostic should cite both the obligated type List(String) and \
         the impl's List(Int); got: {errs:?}"
    );

    // Belt-and-braces: `silt run` must not succeed either.
    let (_stdout, _stderr, ok) = run_silt_raw("reject_string_list", &src);
    assert!(
        !ok,
        "silt run must fail for List(String) against the List(Int)-only impl"
    );
}

// ── Positive control: the matching instantiation still RUNS ─────────

/// `[1, 2] |> sum_it` through the same where-bound fn must keep
/// typechecking AND executing (prints 3). Guards against the fix
/// over-rejecting the legitimate instantiation.
#[test]
fn alias_concrete_impl_where_bound_runs_matching_elements() {
    let src = format!(
        r#"{BYTES2_PRELUDE}
fn main() {{
    let s = [1, 2] |> sum_it
    println(s)
}}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "List(Int) satisfies the alias-expanded List(Int) impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("accept_int_list", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "3", "sum of [1, 2] through the where-bound");
}

// ── Positive control: generic impls keep matching everything ────────

/// A `for List(a)` impl stores Var args (wildcards); every
/// instantiation — including List(String) — must still dispatch
/// through a where-bound and RUN.
#[test]
fn generic_impl_where_bound_still_dispatches_for_any_elements() {
    let src = r#"
import list
trait Total { fn total(self) -> Int }
trait Total for List(a) { fn total(self) -> Int = list.length(self) }
fn sum_it(x: a) -> Int where a: Total { x.total() }
fn main() { println(sum_it(["a", "b", "c"])) }
"#;
    let errs = type_errors(src);
    assert!(
        errs.is_empty(),
        "generic List(a) impl must satisfy any List(T); got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("generic_impl_runs", src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "3");
}

// ── Partially-concrete parametric alias ─────────────────────────────

const NAMED_PRELUDE: &str = r#"
import map
type Named(a) = Map(String, a)
trait Keys { fn keys_count(self) -> Int }
trait Keys for Named(a) { fn keys_count(self) -> Int = map.length(self) }
fn count_it(x: b) -> Int where b: Keys { x.keys_count() }
"#;

/// `trait Keys for Named(a)` (= `Map(String, a)`) must reject a
/// `Map(Int, Bool)` obligation at check time: the concrete `String`
/// key position mismatches, while the `a` value position is a
/// wildcard. Verified pre-fix: this passed check and dispatched.
#[test]
fn parametric_alias_where_bound_rejects_mismatched_key_type() {
    let src = format!(
        r#"{NAMED_PRELUDE}
fn main() {{ println(count_it(#{{ 1: true, 2: false }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.iter()
            .any(|e| e.contains("does not implement trait 'Keys'")),
        "Map(Int, Bool) obligated against the Map(String, a) impl MUST \
         reject at check time; got: {errs:?}"
    );
}

/// ... while a `Map(String, Int)` obligation matches (String == String,
/// value slot defers on the impl's Var) and RUNS end-to-end.
#[test]
fn parametric_alias_where_bound_runs_matching_key_type() {
    let src = format!(
        r#"{NAMED_PRELUDE}
fn main() {{ println(count_it(#{{ "a": 1, "b": 2 }})) }}
"#
    );
    let errs = type_errors(&src);
    assert!(
        errs.is_empty(),
        "Map(String, Int) satisfies the Map(String, a) impl; got: {errs:?}"
    );
    let (stdout, stderr, ok) = run_silt_raw("named_alias_runs", &src);
    assert!(ok, "silt run must succeed; stderr: {stderr}");
    assert_eq!(stdout.trim(), "2");
}
