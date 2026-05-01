//! Round-65 lock: typechecker enforces `fn(...) !{...} { ... }` lambda
//! effect annotations. Mirrors `register_fn_decl`'s narrowing path —
//! when the annotation is narrower than the body's inferred effects,
//! emit `effect '!{X}' not declared in lambda`.
//!
//! Trailing-closure form `{ params -> body }` has no syntactic slot
//! for an effect annotation (`is_annotated == false` always), so this
//! branch is a no-op there: trailing closures stay inferred-only,
//! exactly as before.
//!
//! Strategy:
//!   - Runtime lock (CLI subprocess): `silt check` against a lambda
//!     whose body uses `fs.list_dir` but whose annotation declares
//!     only `!{io}` rejects with the exact diagnostic shape.
//!   - Negative cover: an annotated lambda whose body fits within the
//!     declared set typechecks clean (regression guard).
//!   - Trailing closure cover: trailing-closure form with body using
//!     `println` (which is `!{io}`) typechecks clean — no synthetic
//!     annotation gets invented.
//!   - Un-annotated cover: bare `fn() { ... }` (no `!{...}`) carrying
//!     unrelated effects typechecks clean (legacy code unaffected).

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
    let dir = std::env::temp_dir().join("silt_lambda_effect_enforcement_tests");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{prefix}_{n}.silt"));
    fs::write(&path, content).unwrap();
    path
}

fn check_stderr(path: &PathBuf) -> (bool, String) {
    let output = silt_cmd().arg("check").arg(path).output().expect("silt check");
    (output.status.success(), String::from_utf8_lossy(&output.stderr).to_string())
}

#[test]
fn annotated_lambda_with_body_exceeding_declared_set_is_rejected() {
    let path = temp_silt_file(
        "annotated_lambda_bad",
        "import fs\n\
         \n\
         fn main() {\n\
             let f = fn() !{io} {\n\
                 let _ = fs.list_dir(\"/tmp\")\n\
                 ()\n\
             }\n\
             let _ = f\n\
         }\n",
    );
    let (success, stderr) = check_stderr(&path);
    assert!(
        !success,
        "expected typecheck failure for annotated lambda whose body uses fs but declares only !{{io}}"
    );
    assert!(
        stderr.contains("not declared in lambda"),
        "expected 'not declared in lambda' wording, got stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("!{fs}"),
        "expected the offending effect (!{{fs}}) to be named in the diagnostic, got stderr=`{stderr}`"
    );
    assert!(
        stderr.contains("signature declares !{io}"),
        "expected 'signature declares !{{io}}' tail, got stderr=`{stderr}`"
    );
}

#[test]
fn annotated_lambda_with_body_within_declared_set_typechecks() {
    let path = temp_silt_file(
        "annotated_lambda_good",
        "fn main() {\n\
             let f = fn() !{io} {\n\
                 println(\"hi\")\n\
             }\n\
             f()\n\
         }\n",
    );
    let (success, stderr) = check_stderr(&path);
    assert!(
        success,
        "annotated lambda whose body stays within declared !{{io}} should typecheck; stderr=`{stderr}`"
    );
}

#[test]
fn trailing_closure_form_is_not_subject_to_lambda_enforcement() {
    // Round-65 design call A: trailing closures `{ params -> body }`
    // have no syntactic slot for `!{...}` annotation. Effects are
    // inferred from the body, never enforced. The lambda enforcement
    // branch must skip them (is_annotated is always false for the
    // trailing-closure parser path).
    let path = temp_silt_file(
        "trailing_closure",
        "import list\n\
         \n\
         fn main() {\n\
             let xs = [1, 2, 3]\n\
             let _ = xs |> list.map { x -> x * 2 }\n\
         }\n",
    );
    let (success, stderr) = check_stderr(&path);
    assert!(
        success,
        "trailing-closure form should typecheck without enforcement; stderr=`{stderr}`"
    );
}

#[test]
fn unannotated_fn_expression_lambda_is_not_subject_to_enforcement() {
    // Bare `fn() { ... }` (no `!{...}`) has `is_annotated == false`.
    // Existing programs that use unannotated fn-expressions are
    // untouched — only the explicit-annotation case bites.
    let path = temp_silt_file(
        "unannotated_fn_expr",
        "import fs\n\
         \n\
         fn main() {\n\
             let f = fn() {\n\
                 let _ = fs.list_dir(\"/tmp\")\n\
                 ()\n\
             }\n\
             let _ = f\n\
         }\n",
    );
    let (success, stderr) = check_stderr(&path);
    assert!(
        success,
        "unannotated fn-expression should typecheck even when body has effects; stderr=`{stderr}`"
    );
}

#[test]
fn lambda_full_five_effects_annotation_round_trips_through_typecheck() {
    // Regression guard for the existing
    // `effect_lambda_round_trip_tests::lambda_full_five_effects_annotation_survives_fmt_round_trip`:
    // a lambda annotated with all five effects must still typecheck
    // when the body uses only one of them. Pre-round-65 this was a
    // formatter-only round-trip; with enforcement on, the same input
    // must remain valid (declared !{...all five...} ⊇ body !{io}).
    let path = temp_silt_file(
        "lambda_full_five",
        "fn main() {\n\
             let f = fn() !{io, fs, net, time, random} {\n\
                 println(\"hi\")\n\
             }\n\
             f()\n\
         }\n",
    );
    let (success, stderr) = check_stderr(&path);
    assert!(
        success,
        "lambda annotated with full five-effect set must remain valid when body subsets it; stderr=`{stderr}`"
    );
}
