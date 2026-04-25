//! Built-in enum and record auto-derive synthesis tests.
//!
//! Round-62 follow-up: the auto-derive synthesis pass
//! (`synthesize_auto_derive_impls` in `src/typechecker/mod.rs`) was
//! extended to walk `self.enums` / `self.records` and emit the same
//! `TraitImpl` AST for built-in types as it already does for
//! user-declared types. After this round, every built-in enum and
//! record gets a real `<TypeName>.<method>` global at compile time
//! for each policy-permitted built-in trait (Display / Compare /
//! Equal / Hash) — `Op::CallMethod`'s qualified-global lookup finds
//! the synthesized method directly, never falling through to
//! `dispatch_trait_method`.
//!
//! These tests exercise the synth path end-to-end via the `silt run`
//! CLI. The companion test file
//! `tests/auto_derive_dead_arm_proof_tests.rs` flipped its
//! asymmetry-lock test (`builtin_enums_route_through_synth_global`)
//! to assert all six dispatch counters stay zero after a built-in
//! barrage; this file checks the SEMANTIC outputs.

use std::process::Command;

fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_builtin_synth_{label}.silt"));
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

// ── 1. Built-in enums: behaviour through synth global ──────────────

#[test]
fn weekday_compare_via_synth_global() {
    // `cmp_gen(Monday, Friday)` returns -1 because Monday is declared
    // first in the Weekday enum. The synth-generated `Weekday.compare`
    // global delegates to declaration-order ordinals.
    let out = run_silt_ok(
        "weekday_compare",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(Monday, Friday)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

#[test]
fn result_equal_with_args() {
    // Ok(1).equal(Ok(1)) -> true; Ok(1).equal(Ok(2)) -> false.
    // The synth-generated `Result.equal` global compares variants by
    // tag then field-wise. (Compare is excluded from Result by
    // policy — `non_ordering_traits` — so we exercise Equal here.)
    let out = run_silt_ok(
        "result_equal",
        r#"
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    println(eq_gen(Ok(1), Ok(1)))
    println(eq_gen(Ok(1), Ok(2)))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn option_display_via_synth() {
    // `Some(42).display()` emits `Some(42)` (canonical synth body
    // wraps each field's `.display()` in parens). `None.display()`
    // returns the bare tag.
    let out = run_silt_ok(
        "option_display",
        r#"
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    println(d(Some(42)))
    let n: Option(Int) = None
    println(d(n))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["Some(42)", "None"]);
}

#[test]
fn http_method_compare() {
    // GET, POST, PUT, ... are declared in that order (see
    // `register_http_builtins` in `src/typechecker/builtins/http.rs`).
    // GET < POST → -1.
    let out = run_silt_ok(
        "http_method_compare",
        r#"
import http
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() { println(cmp_gen(GET, POST)) }
"#,
    );
    assert_eq!(out.trim(), "-1");
}

#[test]
fn parse_error_equal() {
    // ParseEmpty.equal(ParseEmpty) is true; ParseEmpty.equal
    // (ParseOverflow) is false. ParseError is a non-generic built-in
    // enum registered via `errors.rs::register_enum`.
    let out = run_silt_ok(
        "parse_error_equal",
        r#"
import int
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    println(eq_gen(ParseEmpty, ParseEmpty))
    println(eq_gen(ParseEmpty, ParseOverflow))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

#[test]
fn step_compare_via_synth() {
    // Step(a) has variants Stop(a), Continue(a) declared in that
    // order. `Stop(1).compare(Continue(1))` -> -1 (Stop's ordinal is
    // 0, Continue's is 1, so Stop < Continue).
    let out = run_silt_ok(
        "step_compare",
        r#"
import list
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Stop(1), Continue(1)))
}
"#,
    );
    assert_eq!(out.trim(), "-1");
}

// ── 2. Built-in records: behaviour through synth global ────────────

#[test]
fn date_compare_via_synth() {
    // Date is a built-in record (year, month, day) registered by
    // `time.rs::register_time_builtins`. Compare is supportable
    // because every field is Int, which has Compare. The synth
    // produces lex-ordered field comparison.
    let out = run_silt_ok(
        "date_compare",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    match time.date(2025, 1, 15) {
        Ok(early) -> match time.date(2025, 6, 30) {
            Ok(late) -> println(cmp_gen(early, late))
            Err(_) -> ()
        }
        Err(_) -> ()
    }
}
"#,
    );
    assert_eq!(out.trim(), "-1");
}

#[test]
fn date_display_via_synth() {
    // `Date { year: 2025, month: 1, day: 15 }.display()` emits the
    // canonical `Date { year: 2025, month: 1, day: 15 }` rendering.
    let out = run_silt_ok(
        "date_display",
        r#"
import time
fn d(a: a) -> String where a: Display { a.display() }
fn main() {
    match time.date(2025, 1, 15) {
        Ok(date) -> println(d(date))
        Err(_) -> ()
    }
}
"#,
    );
    assert_eq!(out.trim(), "Date { year: 2025, month: 1, day: 15 }");
}

#[test]
fn datetime_equal_via_synth() {
    // DateTime contains nested Date and Time records. Equal synth
    // recurses via `.equal()` on each field — which works because
    // `register_auto_derived_impls_for(time, Date/Time/DateTime, all_auto)`
    // pre-stamps trait_impl_set for the inner records. Two distinct
    // DateTime values must not compare equal.
    let out = run_silt_ok(
        "datetime_equal",
        r#"
import time
fn eq_gen(a: a, b: a) -> Bool where a: Equal { a.equal(b) }
fn main() {
    match time.date(2025, 1, 15) {
        Ok(d1) -> match time.time(8, 30, 0) {
            Ok(t1) -> match time.time(9, 0, 0) {
                Ok(t2) -> {
                    let dt1 = time.datetime(d1, t1)
                    let dt2 = time.datetime(d1, t2)
                    println(eq_gen(dt1, dt1))
                    println(eq_gen(dt1, dt2))
                }
                Err(_) -> ()
            }
            Err(_) -> ()
        }
        Err(_) -> ()
    }
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["true", "false"]);
}

// ── 3. Coverage tests: every supportable built-in has a synth impl
//
// Walk `self.enums` and `self.records` via a typechecker fingerprint
// helper; for every built-in entry, assert the policy-permitted
// trait stamps are present in `trait_impl_set`. The synth pass keys
// off `trait_impl_set`, so a missing stamp implies missing synth.

#[test]
fn every_builtin_enum_has_stamps_for_policy_permitted_traits() {
    use silt::typechecker::__trait_init_fingerprint_check_program;
    let (impls, _) = __trait_init_fingerprint_check_program();
    // Built-in enums that should have all four built-in trait stamps
    // (every variant arg type is Compare/Equal/Hash/Display-able).
    for type_name in [
        "Step",
        "ChannelResult",
        "Method",
        "Weekday",
        "IoError",
        "JsonError",
        "TomlError",
        "ParseError",
        "HttpError",
        "RegexError",
        "PgError",
        "TcpError",
        "TimeError",
        "BytesError",
        "ChannelError",
    ] {
        for trait_name in ["Equal", "Compare", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(
                impls.contains(&key),
                "expected built-in enum stamp {key} in trait_impl_set;\n  present: {:?}",
                impls
                    .iter()
                    .filter(|s| s.ends_with(&format!(":{type_name}")))
                    .collect::<Vec<_>>(),
            );
        }
    }

    // Option/Result: Equal/Hash/Display only (Compare excluded by
    // `non_ordering_traits` — see `tests/trait_init_parity_tests.rs`).
    for type_name in ["Option", "Result"] {
        for trait_name in ["Equal", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(impls.contains(&key), "expected stamp {key}");
        }
        let compare_key = format!("Compare:{type_name}");
        assert!(
            !impls.contains(&compare_key),
            "did not expect stamp {compare_key} (excluded by policy)",
        );
    }
}

#[test]
fn every_builtin_record_has_stamps_for_policy_permitted_traits() {
    use silt::typechecker::__trait_init_fingerprint_check_program;
    let (impls, _) = __trait_init_fingerprint_check_program();
    // time records — all four built-in trait stamps via
    // `register_auto_derived_impls_for(time, &[...], all_auto_traits)`.
    for type_name in ["Instant", "Date", "Time", "DateTime", "Duration"] {
        for trait_name in ["Equal", "Compare", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(
                impls.contains(&key),
                "expected built-in record stamp {key} in trait_impl_set",
            );
        }
    }
    // FileStat — same (via `fs.rs::register_fs_builtins`).
    for trait_name in ["Equal", "Compare", "Hash", "Display"] {
        let key = format!("{trait_name}:FileStat");
        assert!(
            impls.contains(&key),
            "expected built-in record stamp {key}",
        );
    }
    // Response/Request — Equal/Hash/Display only (Map field blocks
    // Compare). Stamped via `register_builtin_trait_impls`.
    for type_name in ["Response", "Request"] {
        for trait_name in ["Equal", "Hash", "Display"] {
            let key = format!("{trait_name}:{type_name}");
            assert!(impls.contains(&key), "expected stamp {key}");
        }
        let compare_key = format!("Compare:{type_name}");
        assert!(
            !impls.contains(&compare_key),
            "did not expect stamp {compare_key} (Map field has no Compare)",
        );
    }
}

// ── 4. Method-table coverage: every stamped pair compiles to a global
//
// The end-state check: after `synthesize_auto_derive_impls` runs and
// the compiler emits each TraitImpl, the qualified-global table has
// a `<TypeName>.<method>` entry for every (trait, type) stamp we
// expect. We run a tiny silt program that imports the relevant
// modules and lets the compile pass run to completion; if any synth
// global is missing, `Op::CallMethod` would later miss it. This
// test exercises the compile path itself.

#[test]
fn weekday_method_call_resolves_through_global() {
    // Lock that the synth-emitted `Weekday.compare` global is
    // findable at runtime by the qualified-call path. Monday/Friday
    // are bound as `Value::Variant`, so direct member-style call
    // (`Monday.compare(Friday)`) compiles as a module-style global
    // lookup `Monday.compare` and would fail — variants reach
    // `compare` via `Op::CallMethod`, not module-style fields. The
    // proper qualified call form is therefore through the where-
    // bound dispatch helper, which in turn calls
    // `Op::CallMethod -> Weekday.compare`. Cover the three orderings
    // to lock declaration-order semantics.
    let out = run_silt_ok(
        "weekday_compare_via_bound",
        r#"
import time
fn cmp_gen(a: a, b: a) -> Int where a: Compare { a.compare(b) }
fn main() {
    println(cmp_gen(Monday, Friday))
    println(cmp_gen(Friday, Monday))
    println(cmp_gen(Monday, Monday))
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

#[test]
fn date_method_call_resolves_through_global() {
    // Same as above for built-in records: `date.compare(other)`
    // resolves to the synth `Date.compare` global.
    let out = run_silt_ok(
        "date_direct_compare",
        r#"
import time
fn main() {
    match time.date(2025, 1, 15) {
        Ok(a) -> match time.date(2025, 1, 20) {
            Ok(b) -> {
                println(a.compare(b))
                println(b.compare(a))
                println(a.compare(a))
            }
            Err(_) -> ()
        }
        Err(_) -> ()
    }
}
"#,
    );
    let lines: Vec<&str> = out.trim().split('\n').collect();
    assert_eq!(lines, vec!["-1", "1", "0"]);
}

// ── Performance smoke (helps gauge synth overhead) ─────────────────
//
// Print compile/typecheck/vm-run timings for a trivial program. This
// is informational, not assertional — surfacing here so any future
// "synth pass got slow" regression appears next to the test that
// motivated it (`scheduler::test_support::tests::run_trial_returns_main_value`
// has a 2-second wall-clock budget; if synth pushes past that, this
// trace pinpoints the bottleneck).
#[test]
fn timing_trivial_program_compile_run() {
    use std::sync::Arc;
    use std::time::Instant;
    let source = "fn main() { 42 }";
    let t0 = Instant::now();
    let tokens = silt::lexer::Lexer::new(source).tokenize().unwrap();
    let t1 = Instant::now();
    let mut program = silt::parser::Parser::new(tokens).parse_program().unwrap();
    let t2 = Instant::now();
    let _ = silt::typechecker::check(&mut program);
    let t3 = Instant::now();
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).unwrap();
    let t4 = Instant::now();
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    let _ = vm.run(script);
    let t5 = Instant::now();
    eprintln!("=== timings ===");
    eprintln!("  lex:        {:?}", t1 - t0);
    eprintln!("  parse:      {:?}", t2 - t1);
    eprintln!("  typecheck:  {:?}", t3 - t2);
    eprintln!("  compile:    {:?}", t4 - t3);
    eprintln!("  vm.run:     {:?}", t5 - t4);
    eprintln!("  total:      {:?}", t5 - t0);
}
