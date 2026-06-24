//! Round 97 regression locks — container-of-`Fn` compare/equality gate.
//!
//! FINDING (BROKEN, type-system soundness): ordering (`<` `>` `<=` `>=`)
//! and equality (`==` `!=`) on a CONTAINER of function-shaped values
//! (List/Range/Tuple/Set/Map whose element / component / value type is
//! `Fn`) was wrongly ACCEPTED by the typechecker, then at runtime
//! laundered into ASLR-nondeterministic pointer-address ordering
//! (`VmClosure` ordered by `Arc::as_ptr`, src/value.rs).
//!
//! This is the same bug-class round 3 fixed for bare `Fn` operands and
//! round 93 fixed for nominal records / enums — the container HEADS
//! (`List(_)`, `Range(_)`, `Tuple`, `Map`, `Set`) slip through the
//! structural shape gate `is_valid_compare_operand` because that gate
//! does not recurse into element types.
//!
//! FIX: `operand_builtin_trait_violation` (src/typechecker/mod.rs) now
//! recurses into container element / component / value types via the
//! round-93 `gate_field_supports_trait` walker (which bottoms out at
//! `Type::Fun(..) => false`) and rejects the operand with a message
//! mirroring the round-93 nominal shape:
//!   `type 'List(Fn(...) -> ...)' cannot derive 'Compare': element
//!    type is not comparable`
//!
//! Every rejection test below FAILS on the pre-fix code (the program
//! typechecked and reached the VM, ordering closures by pointer
//! address) and PASSES post-fix. The positive-control tests guard
//! against over-rejection (the gate's failure mode).

use std::process::Command;

use silt::typechecker;
use silt::types::Severity;

/// Typecheck `input` in-process and return the Error-severity messages.
fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let errors = typechecker::check(&mut program);
    errors
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

/// Assert that typecheck rejects `input` with a message containing all
/// of `needles`.
fn assert_rejected(label: &str, input: &str, needles: &[&str]) {
    let errs = type_errors(input);
    assert!(
        !errs.is_empty(),
        "{label}: expected a typecheck rejection, got none"
    );
    for needle in needles {
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "{label}: expected an error containing {needle:?}, got: {errs:?}"
        );
    }
}

/// Assert that typecheck accepts `input` (no Error-severity messages).
fn assert_accepted(label: &str, input: &str) {
    let errs = type_errors(input);
    assert!(
        errs.is_empty(),
        "{label}: expected clean typecheck, got: {errs:?}"
    );
}

/// Drive the full lex → parse → typecheck → compile → VM pipeline via
/// the `silt run` CLI and return (stdout, stderr, success).
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round97_container_fn_{label}.silt"));
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

// ── REJECTIONS: container of Fn under ordering / equality ──────────────

#[test]
fn list_of_fn_ordering_is_rejected() {
    // The exact repro from the finding.
    let src = r#"
fn main() {
  let xs = [fn(x) { x }]
  let ys = [fn(x) { x }]
  print(xs < ys)
}
"#;
    assert_rejected(
        "list_of_fn_ordering",
        src,
        &["cannot derive 'Compare'", "element type is not comparable"],
    );
}

#[test]
fn list_of_fn_equality_is_rejected() {
    let src = r#"
fn main() {
  let xs = [fn(x) { x }]
  let ys = [fn(x) { x }]
  print(xs == ys)
}
"#;
    assert_rejected(
        "list_of_fn_equality",
        src,
        &["cannot derive 'Equal'", "element type is not equatable"],
    );
}

#[test]
fn tuple_of_fn_equality_is_rejected() {
    // Tuples are equality-only (no ordering), so exercise `==`.
    let src = r#"
fn main() {
  let a = (1, fn(x) { x })
  let b = (1, fn(x) { x })
  print(a == b)
}
"#;
    assert_rejected(
        "tuple_of_fn_equality",
        src,
        &["cannot derive 'Equal'", "element type is not equatable"],
    );
}

#[test]
fn rejected_program_never_reaches_runtime() {
    // Run the ordering repro through the real CLI: it must fail (the
    // typecheck rejects it) and never print a (pointer-address) Bool.
    let src = r#"
fn main() {
  let xs = [fn(x) { x }]
  let ys = [fn(x) { x }]
  print(xs < ys)
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("ordering_run", src);
    assert!(
        !ok,
        "expected `silt run` to fail at typecheck; stdout={stdout:?} stderr={stderr:?}"
    );
    // No `true`/`false` ever printed: the program is rejected before the VM.
    assert!(
        stdout.trim().is_empty(),
        "rejected program should produce no stdout, got {stdout:?}"
    );
    assert!(
        stderr.contains("cannot derive 'Compare'") || stderr.contains("comparable"),
        "expected the precise rejection on stderr, got {stderr:?}"
    );
}

// ── POSITIVE CONTROLS: orderable / equatable containers stay valid ─────

#[test]
fn list_of_int_ordering_still_typechecks() {
    let src = r#"
fn main() {
  print([1, 2, 3] < [4, 5, 6])
}
"#;
    assert_accepted("list_of_int_ordering", src);
}

#[test]
fn list_of_int_ordering_still_runs() {
    let src = r#"
fn main() {
  print([1, 2, 3] < [4, 5, 6])
}
"#;
    let (stdout, stderr, ok) = run_silt_raw("int_ordering_run", src);
    assert!(ok, "positive control failed: stderr={stderr:?}");
    assert_eq!(stdout.trim(), "true", "stderr={stderr:?}");
}

#[test]
fn tuple_and_set_of_orderable_values_still_typecheck() {
    // Tuple equality of equatable components.
    let tuple_src = r#"
fn main() {
  print((1, "a") == (1, "b"))
}
"#;
    assert_accepted("tuple_of_eq", tuple_src);

    // Set equality of equatable elements (`#[ ]` is the set literal).
    let set_src = r#"
fn main() {
  print(#[1, 2, 3] == #[3, 2, 1])
}
"#;
    assert_accepted("set_of_eq", set_src);
}
