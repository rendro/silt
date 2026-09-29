//! Round-92 lock tests: deep/wide ASTs must not abort with a Rust
//! stack overflow.
//!
//! Pre-fix, two small *valid* programs killed the process with
//! `thread 'silt-main' has overflowed its stack / fatal runtime error:
//! stack overflow, aborting` (SIGABRT):
//!
//!   (a) a ~250-term `1 + 1 + ...` chain — the Pratt loop builds
//!       left-leaning binop chains iteratively, so AST depth is
//!       unbounded downstream of the parser's MAX_DEPTH guard, and the
//!       typechecker/compiler walkers recurse once per level on an
//!       8 MiB silt-main stack;
//!   (b) a 70-field record declaration — auto-derive synthesized
//!       left-leaning compare/equal/hash/display chains over the
//!       fields, turning a wide record into case (a) on `silt check`.
//!
//! Fixed by (1) emitting balanced trees / flat statement sequences
//! from `src/typechecker/auto_derive.rs` so derive depth is
//! O(log n)/O(1), and (2) raising the silt-main stack reserve in
//! `src/main.rs` (virtual-only cost) so user-written chain thresholds
//! rise by an order of magnitude.
//!
//! All tests run the compiled CLI binary so they exercise the *real*
//! silt-main thread (its name + stack size are set in `src/main.rs`),
//! not the small default stack of a libtest worker thread.

use std::process::Command;

/// Run `silt <subcmd> <generated program>` via the compiled binary and
/// return (stdout, stderr, success).
fn silt_cli(label: &str, subcmd: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round92_deep_{label}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg(subcmd)
        .arg(&tmp)
        .output()
        .expect("spawn silt");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status.success())
}

fn assert_clean(label: &str, stderr: &str, ok: bool, stdout: &str) {
    assert!(
        ok,
        "{label}: silt should exit 0; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        !stderr.contains("overflowed its stack") && !stderr.contains("stack overflow"),
        "{label}: must not abort with a Rust stack overflow; stderr={stderr:?}"
    );
}

/// `fn main() {{ let x = 1 + 1 + ... (n terms); println(x) }}`
fn plus_chain_program(n: usize) -> String {
    let terms = vec!["1"; n].join(" + ");
    format!("fn main() {{\n  let x = {terms}\n  println(x)\n}}\n")
}

/// `type R {{ f0: Int, ..., f{{n-1}}: Int }}` plus a trivial main.
fn wide_record_program(n: usize) -> String {
    let fields: Vec<String> = (0..n).map(|i| format!("  f{i}: Int")).collect();
    format!(
        "type R {{\n{}\n}}\n\nfn main() {{\n  println(1)\n}}\n",
        fields.join(",\n")
    )
}

// ── (i) user-written left-leaning chains ─────────────────────────────

/// Pre-fix threshold was between 200 and 250 terms (8 MiB stack).
#[test]
fn plus_chain_250_terms_runs_and_prints_sum() {
    let (stdout, stderr, ok) = silt_cli("chain250", "run", &plus_chain_program(250));
    assert_clean("chain250", &stderr, ok, &stdout);
    assert_eq!(stdout.trim(), "250");
}

/// Well past the old abort threshold — locks the raised stack reserve.
#[test]
fn plus_chain_2000_terms_runs_and_prints_sum() {
    let (stdout, stderr, ok) = silt_cli("chain2000", "run", &plus_chain_program(2000));
    assert_clean("chain2000", &stderr, ok, &stdout);
    assert_eq!(stdout.trim(), "2000");
}

// ── (ii) wide records: auto-derive must not synthesize deep chains ───

/// Pre-fix `silt check` aborted at 70 fields (65 was fine).
#[test]
fn record_70_fields_checks_cleanly() {
    let (stdout, stderr, ok) = silt_cli("rec70", "check", &wide_record_program(70));
    assert_clean("rec70", &stderr, ok, &stdout);
}

#[test]
fn record_200_fields_checks_cleanly() {
    let (stdout, stderr, ok) = silt_cli("rec200", "check", &wide_record_program(200));
    assert_clean("rec200", &stderr, ok, &stdout);
}

/// Balanced derives make depth O(log n), so width should be a complete
/// non-issue — lock that with a 500-field record per the round-92
/// acceptance bar.
#[test]
fn record_500_fields_checks_cleanly() {
    let (stdout, stderr, ok) = silt_cli("rec500", "check", &wide_record_program(500));
    assert_clean("rec500", &stderr, ok, &stdout);
}

// ── semantics locks: balanced folding must not change derive results ─
//
// Lexicographic compare is associative in the "first non-zero wins"
// monoid, so the balanced fold groups differently but the *first*
// (declaration-order) non-equal field still decides — these tests lock
// that with records/values where field order determines the outcome.

/// The first differing field in *declaration order* must decide the
/// comparison, even when later fields point the other way.
#[test]
fn record_compare_first_differing_field_wins_in_declaration_order() {
    let src = r#"
type P { a: Int, b: Int, c: Int }
fn cmp_gen(x: t, y: t) -> Int where t: Compare { x.compare(y) }
fn main() {
  -- a decides (later fields disagree loudly)
  println(cmp_gen(P { a: 1, b: 9, c: 9 }, P { a: 2, b: 0, c: 0 }))
  -- a ties, b decides (c disagrees)
  println(cmp_gen(P { a: 1, b: 2, c: 9 }, P { a: 1, b: 3, c: 0 }))
  -- a and b tie, c decides
  println(cmp_gen(P { a: 1, b: 2, c: 5 }, P { a: 1, b: 2, c: 3 }))
  -- full tie
  println(cmp_gen(P { a: 1, b: 2, c: 3 }, P { a: 1, b: 2, c: 3 }))
}
"#;
    let (stdout, stderr, ok) = silt_cli("cmp_order", "run", src);
    assert_clean("cmp_order", &stderr, ok, &stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["-1", "-1", "1", "0"]);
}

/// End-to-end on a *wide* record: compare/equal/display all flow
/// through the new balanced shapes at runtime. The two instances
/// differ only in the LAST field so every combine node on the path is
/// exercised, and the display string locks separator/order fidelity.
#[test]
fn wide_record_derives_run_with_correct_semantics() {
    const N: usize = 80;
    let fields: Vec<String> = (0..N).map(|i| format!("  f{i}: Int")).collect();
    let mk = |last: i64| {
        let inits: Vec<String> = (0..N)
            .map(|i| {
                if i == N - 1 {
                    format!("f{i}: {last}")
                } else {
                    format!("f{i}: {i}")
                }
            })
            .collect();
        format!("W {{ {} }}", inits.join(", "))
    };
    let src = format!(
        r#"
type W {{
{fields}
}}
fn cmp_gen(x: t, y: t) -> Int where t: Compare {{ x.compare(y) }}
fn eq_gen(x: t, y: t) -> Bool where t: Equal {{ x.equal(y) }}
fn show(x: t) -> String where t: Display {{ x.display() }}
fn main() {{
  let lo = {lo}
  let hi = {hi}
  let lo2 = {lo}
  println(cmp_gen(lo, hi))
  println(cmp_gen(hi, lo))
  println(cmp_gen(lo, lo2))
  println(eq_gen(lo, hi))
  println(eq_gen(lo, lo2))
  println(show(lo))
}}
"#,
        fields = fields.join(",\n"),
        lo = mk(1000),
        hi = mk(2000),
    );
    let (stdout, stderr, ok) = silt_cli("wide_runtime", "run", &src);
    assert_clean("wide_runtime", &stderr, ok, &stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "-1", "lo < hi (last field decides)");
    assert_eq!(lines[1], "1", "hi > lo");
    assert_eq!(lines[2], "0", "identical records compare 0");
    assert_eq!(lines[3], "false");
    assert_eq!(lines[4], "true");
    // Display: declaration order, ", "-separated, "W { ... }" frame —
    // identical to the old left-fold concat (string concat is
    // associative, so balanced grouping yields the same string).
    let display = lines[5];
    assert!(display.starts_with("W { f0: 0, f1: 1, "), "got {display:?}");
    assert!(
        display.ends_with(&format!("f{}: {}, f{}: 1000 }}", N - 2, N - 2, N - 1)),
        "got {display:?}"
    );
}

/// Enum variants with multiple args go through the same balanced
/// builders (`build_lex_compare_chain` / the `&&`-join): lock the
/// lexicographic order on a 3-arg variant.
#[test]
fn enum_variant_lex_compare_first_arg_wins() {
    let src = r#"
type T { V(Int, Int, Int) }
fn cmp_gen(x: t, y: t) -> Int where t: Compare { x.compare(y) }
fn eq_gen(x: t, y: t) -> Bool where t: Equal { x.equal(y) }
fn main() {
  println(cmp_gen(V(1, 9, 9), V(2, 0, 0)))
  println(cmp_gen(V(1, 2, 9), V(1, 3, 0)))
  println(cmp_gen(V(1, 2, 5), V(1, 2, 3)))
  println(cmp_gen(V(1, 2, 3), V(1, 2, 3)))
  println(eq_gen(V(1, 2, 3), V(1, 2, 3)))
  println(eq_gen(V(1, 2, 3), V(1, 2, 4)))
}
"#;
    let (stdout, stderr, ok) = silt_cli("enum_lex", "run", src);
    assert_clean("enum_lex", &stderr, ok, &stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["-1", "-1", "1", "0", "true", "false"]);
}
