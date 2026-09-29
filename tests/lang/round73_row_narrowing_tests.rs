//! Round 73 B1 (BROKEN, soundness): scheme narrowing was skipped when
//! row-polymorphic body inference preserved a function's `vars.len()`.
//!
//! Repro: `fn pluck(r) = r.zzznosuchfield` followed by a call passing
//! a closed `{a: Int, b: Int}` record. Pre-fix:
//!   - Pass 2 generalizes `pluck` to `Fn(α) -> β` (vars=[α, β]).
//!   - Body inference unifies `α` with `AnonRecord{zzznosuchfield: β,
//!     ...γ}` — one tyvar consumed, one row tyvar introduced.
//!   - The legacy `vars.len()` gate compared 2 vs 2 and skipped the
//!     narrowing branch entirely. The deferred `pending_field_accesses`
//!     entry stayed wired to the pass-2 tyvar (still polymorphic per
//!     `apply`), so `finalize_deferred_checks` saw `Type::Var(_)` and
//!     left the bogus access alone.
//!   - The program crashed at runtime with `record has no field
//!     'zzznosuchfield'`.
//!
//! Post-fix: a structural narrowing detector (`scheme_narrowed`) catches
//! the Var → AnonRecord substitution and re-runs the body-check pass
//! with the narrowed scheme, surfacing the field error at typecheck
//! time. Annotated forms (`r: {a: Int, b: Int}`) were already rejected
//! correctly; this lock pins the unannotated path.

use silt::typechecker;
use silt::types::Severity;
use std::process::Command;

/// Drive the full public pipeline (lexer → parser → typechecker →
/// compiler → VM) the same way `silt run` does, by spawning the built
/// `silt` binary. Returns `(stdout, stderr, success)` so callers can
/// assert that a type-rejected program never reaches runtime.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let tmp = std::env::temp_dir().join(format!("silt_round73_row_{label}.silt"));
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

/// Lock test: the unannotated row-poly form must produce a typecheck
/// error referencing the bogus field. Without B1's structural narrowing
/// gate, this program slipped through to runtime.
#[test]
fn unannotated_row_poly_bogus_field_rejected_at_typecheck() {
    let errs = type_errors(
        r#"
fn pluck(r) = r.zzznosuchfield
fn main() {
    let p = {a: 1, b: 2}
    println(pluck(p))
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "expected at least one typecheck error rejecting the bogus field; \
         got none — pluck(p) slipped through to runtime"
    );
    // Wording: the row-poly receiver's open side carries `zzznosuchfield`
    // while the call site supplies a closed `{a, b}` record. The unify
    // arm at src/typechecker/mod.rs:1061 emits "record open side has
    // fields not present in closed target: zzznosuchfield"; the older
    // annotated path emits "anon record has no field 'zzznosuchfield'".
    // Either wording (or any future replacement that still names the
    // field) closes the soundness hole — so we assert on the field name.
    assert!(
        errs.iter().any(|e| e.contains("zzznosuchfield")),
        "expected an error mentioning the bogus field name; got: {errs:?}"
    );
}

/// Belt-and-suspender: the existing annotated form was already rejected.
/// This check ensures the fix does not regress that path.
#[test]
fn annotated_form_still_rejected_unchanged() {
    let errs = type_errors(
        r#"
fn pluck(r: {a: Int, b: Int}) = r.zzznosuchfield
fn main() {
    let p = {a: 1, b: 2}
    println(pluck(p))
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| e.contains("anon record has no field") && e.contains("zzznosuchfield")),
        "expected the annotated-path 'anon record has no field' error; got: {errs:?}"
    );
}

/// Row-polymorphic pluck with a VALID field must still typecheck — the
/// fix must not over-trigger and reject legitimate row-poly use.
#[test]
fn unannotated_row_poly_valid_field_still_passes() {
    let errs = type_errors(
        r#"
fn pluck(r) = r.a
fn main() {
    let p = {a: 1, b: 2}
    println(pluck(p))
}
"#,
    );
    assert!(
        errs.is_empty(),
        "valid row-poly access r.a should typecheck cleanly; got: {errs:?}"
    );
}

/// End-to-end: drive the full lex → parse → typecheck → compile pipeline
/// and assert that compilation does not produce a runnable program when
/// the typecheck rejects the row-poly bogus access. This guards against
/// a future regression where the typechecker error is correctly emitted
/// but the compiler still hands out bytecode (which would let the
/// runtime crash even though the typechecker did its job).
#[test]
fn unannotated_row_poly_bogus_field_blocks_compile() {
    let src = r#"
fn pluck(r) = r.zzznosuchfield
fn main() {
    let p = {a: 1, b: 2}
    println(pluck(p))
}
"#;
    let tokens = silt::lexer::Lexer::new(src)
        .tokenize()
        .expect("lexer error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let type_errs = typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errs
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        !hard_errors.is_empty(),
        "typecheck must produce at least one hard error for the bogus field; \
         got: {type_errs:?}"
    );
    // The hard error must surface BEFORE any runtime — we verify by
    // confirming the error mentions the bogus field name.
    assert!(
        hard_errors
            .iter()
            .any(|e| e.message.contains("zzznosuchfield")),
        "typecheck error must mention the bogus field 'zzznosuchfield'; got: {hard_errors:?}"
    );

    // Compile-stage guard (the claim this lock makes): drive the SAME
    // source through the full public pipeline `silt run` uses. The CLI
    // gates on hard type errors (src/cli/pipeline.rs `compile_file_with_options`:
    // it `process::exit(1)`s before handing functions to the VM), so the
    // rejected program must EXIT NON-ZERO, never reach runtime, and print
    // the field error to stderr. If a future regression let the compiler
    // hand out bytecode despite the typecheck error, the program would run
    // and crash — this assertion catches that.
    let (stdout, stderr, ok) = run_silt_raw("bogus_field_blocks_compile", src);
    assert!(
        !ok,
        "silt run must reject the bogus-field program before runtime; \
         instead it succeeded — the compiler handed out runnable bytecode. \
         stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stderr.contains("zzznosuchfield"),
        "the pipeline must surface the field error on stderr; \
         stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "a type-rejected program must never produce runtime output; \
         got stdout={stdout:?}"
    );
}
