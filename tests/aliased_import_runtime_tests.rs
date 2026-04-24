//! Runtime regression tests for aliased builtin-module imports.
//!
//! Background — REGRESSION(18e6e21): round 58 made the typechecker
//! mirror every `{module}.{suffix}` binding under a new alias prefix
//! when the user writes `import list as l`. The commit message claimed
//! "the runtime path was unaffected ... the compiler's own resolution
//! handles aliases correctly". That was WRONG.
//!
//! At compile time, `src/compiler/mod.rs` handles the alias by
//! iterating `module::builtin_module_functions(canonical_name)` — a
//! CURATED list — and emitting `GetGlobal("list.sum") +
//! SetGlobal("l.sum")` pairs for each entry. Any function registered
//! only in a typechecker submodule (e.g. `list.sum`, `list.product`,
//! `string.lines`) was absent from that curated list, so no alias
//! global was ever created. The direct-call path
//! (`list.sum(…)`) works via `Op::CallBuiltin` which bypasses the
//! globals table and dispatches by module prefix, but the aliased
//! call `l.sum(…)` fell through to `GetGlobal("l.sum")` and blew up
//! with `undefined global: l.sum`.
//!
//! Fix: track alias → canonical-builtin-module in the compiler and
//! rewrite `l.sum(…)` as `CallBuiltin("list.sum", …)` in
//! `extract_builtin_name`, matching the typechecker's prefix-mirror
//! invariant (src/typechecker/mod.rs:1107).
//!
//! These tests exercise the runtime via the `silt` CLI so they fail
//! before the compiler-side fix and pass after.

use std::process::Command;

/// Run a Silt source program and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_aliased_import_rt_{label}.silt"));
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

/// The canonical REGRESSION repro: `import list as l` then
/// `l.sum([…])` must run and print "6". Before the fix this failed
/// with `error[runtime]: undefined global: l.sum`.
#[test]
fn aliased_import_l_sum_runs() {
    let out = run_silt_ok(
        "l_sum",
        r#"
import list as l
fn main() { println(l.sum([1, 2, 3])) }
"#,
    );
    assert!(
        out.contains('6'),
        "expected '6' in stdout for l.sum([1,2,3]); got {out:?}"
    );
}

/// Companion repros for the other non-curated `list` submodule
/// functions the finding calls out — `l.product`, `l.sum_float`,
/// `l.product_float`. All four came from the same missing-alias
/// mirror bug.
#[test]
fn aliased_import_list_non_curated_runs() {
    let out = run_silt_ok(
        "list_non_curated",
        r#"
import list as l
fn main() {
  println(l.product([1, 2, 3, 4]))
  println(l.sum_float([1.5, 2.5]))
  println(l.product_float([2.0, 3.0]))
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines.len() >= 3,
        "expected at least 3 output lines; got {out:?}"
    );
    assert!(
        lines[0].contains("24"),
        "l.product([1..4]) = 24; line0={:?}",
        lines[0]
    );
    assert!(
        lines[1].contains('4'),
        "l.sum_float([1.5, 2.5]) = 4(.0); line1={:?}",
        lines[1]
    );
    assert!(
        lines[2].contains('6'),
        "l.product_float([2.0, 3.0]) = 6(.0); line2={:?}",
        lines[2]
    );
}

/// Non-curated `string` submodule function `s.lines` via alias.
/// The finding calls out `s.lines`, `s.last_index_of`, `s.split_at`,
/// `s.starts_with_at`; this test uses `s.lines` which is the
/// representative shape (returns a list). `list.each` drives the
/// iteration since silt has no `for` keyword.
#[test]
fn aliased_import_s_lines_runs() {
    let out = run_silt_ok(
        "s_lines",
        r#"
import string as s
import list
fn main() {
  list.each(s.lines("a\nb"), fn(l) { println(l) })
}
"#,
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["a", "b"],
        "expected a\\nb from s.lines; got {out:?}"
    );
}

/// Regression guard: the curated case `l.map` must still work. This
/// asserts the fix didn't regress the existing alias path that
/// `builtin_module_functions` already covered.
#[test]
fn aliased_import_l_map_still_runs() {
    let out = run_silt_ok(
        "l_map_guard",
        r#"
import list as l
fn main() { println(l.map([1, 2, 3], fn(x) { x + 1 })) }
"#,
    );
    // l.map([1,2,3], +1) = [2, 3, 4] — display varies by list
    // formatter but must contain the elements in order.
    assert!(
        out.contains('2') && out.contains('3') && out.contains('4'),
        "expected 2, 3, 4 in stdout for l.map; got {out:?}"
    );
}
