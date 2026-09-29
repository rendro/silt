//! Lambda effect-annotation round-trip locks.
//!
//! Sibling fix to round 62's `FnDecl` cluster (see
//! `effect_round62_audit_tests::fn_with_all_five_effects_annotation_survives_fmt_round_trip`).
//! The five-bit `EffectSet::TOP` bitset (`0b00011111`, all five
//! effects) is identical at the value level to a hand-written
//! `!{io, fs, net, time, random}` annotation. Round 62 added an
//! `is_annotated: bool` to `FnDecl` to disambiguate "user wrote no
//! annotation" from "user wrote the full set". Lambdas (specifically
//! the `fn(...)` form, parsed by `Parser::parse_fn_expr`) had the
//! same bug — `silt fmt` silently dropped the user's full annotation
//! because the formatter pivoted on `*effects == EffectSet::TOP`.
//!
//! These locks are CLI-level (`silt fmt`) round-trip assertions, not
//! typechecker-pass tests. The typechecker does not yet enforce
//! lambda effect annotations (see `EffectSet` doc on
//! `ExprKind::Lambda` in `src/ast.rs`); enforcement is deliberately
//! out of scope here. The locks pin only the user-visible formatter
//! behaviour: an explicit annotation must round-trip; an absent
//! annotation must not gain one.
//!
//! The trailing-closure form `{ params -> body }` has no syntactic
//! slot for an effect annotation. We lock that the formatter does
//! not invent one.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn silt_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn temp_silt_file(prefix: &str, content: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join("silt_effect_lambda_round_trip");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("{prefix}_{n}.silt");
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn run_fmt(path: &PathBuf) -> String {
    let output = silt_cmd()
        .arg("fmt")
        .arg(path)
        .output()
        .expect("failed to run silt fmt");
    assert!(
        output.status.success(),
        "silt fmt exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(path).expect("read formatted output")
}

// ── Lock 1: full five-effect lambda annotation survives fmt ──────────

#[test]
fn lambda_with_all_five_effects_annotation_survives_fmt_round_trip() {
    // The user wrote `!{io, fs, net, time, random}` on a `fn(...)`
    // lambda expression. Bit-equality would collapse this onto
    // `EffectSet::TOP` (the gradual default), and the formatter
    // previously dropped TOP on the floor (`src/formatter.rs:5429`
    // pre-fix). With the `is_annotated` pivot in place the user's
    // annotation must round-trip through `silt fmt`. The output
    // is the alphabetised render (matching `Display for EffectSet`'s
    // alphabetic ordering, same as the round-62 FnDecl lock).
    let path = temp_silt_file(
        "lambda_full_five_effects",
        r#"let f = fn() !{io, fs, net, time, random} { println("hi") }
"#,
    );

    let formatted = run_fmt(&path);

    // Alphabetic order: fs, io, net, random, time.
    assert!(
        formatted.contains("!{fs, io, net, random, time}"),
        "expected the full five-effect annotation to survive fmt on a lambda, got:\n{formatted}"
    );
    // Belt-and-suspenders: the pre-fix bug dropped the annotation
    // entirely. Make sure the formatted output still has *some* `!{`.
    assert!(
        formatted.contains("!{"),
        "fmt dropped the lambda effect annotation entirely:\n{formatted}"
    );
}

// ── Lock 2: simple single-effect lambda annotation round-trips ───────

#[test]
fn lambda_with_io_only_annotation_survives_fmt_round_trip() {
    // A single-effect annotation does NOT collide with TOP, so it
    // would have round-tripped under the old bit-equality pivot too.
    // The lock ensures the new `is_annotated` pivot does not
    // accidentally suppress the simple case (regression boundary
    // for the fix).
    let path = temp_silt_file(
        "lambda_io_only",
        r#"let f = fn() !{io} { println("hi") }
"#,
    );

    let formatted = run_fmt(&path);

    assert!(
        formatted.contains("!{io}"),
        "expected `!{{io}}` to survive fmt on a lambda, got:\n{formatted}"
    );
}

// ── Lock 3: un-annotated `fn(...)` lambda stays un-annotated ─────────

#[test]
fn lambda_without_annotation_does_not_emit_effects() {
    // No `!{...}` written by the user → `is_annotated == false` → the
    // formatter must NOT invent an annotation (the gradual-rollout
    // permissive default `TOP` is the absence-marker, and emitting
    // `!*` would be both noise and unparseable). Locks the regression
    // boundary opposite to lock 1: the explicit-set fallback must
    // only fire when `is_annotated == true`.
    let path = temp_silt_file(
        "lambda_no_annotation",
        r#"let f = fn() { println("hi") }
"#,
    );

    let formatted = run_fmt(&path);

    assert!(
        !formatted.contains("!{"),
        "fmt invented an effect annotation on an un-annotated lambda:\n{formatted}"
    );
}

// ── Lock 4: trailing-closure form has no annotation slot ─────────────

#[test]
fn trailing_closure_lambda_does_not_emit_effects() {
    // The trailing-closure form `{ -> body }` has no syntactic slot
    // for an effect annotation (parser sets `is_annotated: false`
    // unconditionally at `parse_trailing_closure_as_lambda`). The
    // formatter must not invent one. Pre-fix, the same `== TOP`
    // pivot would also have suppressed emission here (TOP is the
    // default), so this lock guards against a future change that
    // pivots on `is_annotated` but forgets to pre-fill `false` for
    // the closure form.
    let path = temp_silt_file(
        "trailing_closure_no_effects",
        r#"let f = { -> println("hi") }
"#,
    );

    let formatted = run_fmt(&path);

    assert!(
        !formatted.contains("!{"),
        "fmt invented an effect annotation on a trailing-closure lambda:\n{formatted}"
    );
}
