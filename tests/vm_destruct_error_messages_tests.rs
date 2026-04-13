//! Regression tests for GAP #1 (round-23 audit D): destruct runtime
//! errors used to leak internal opcode names (e.g. "DestructList index 2
//! out of bounds (len 2)") directly into user-facing error output. Each
//! `Destruct*` op's error path has been rewritten to describe the
//! problem in user terms ("list destructure: expected at least 3
//! elements, got 2") while preserving the information content (indices,
//! lengths, actual type).
//!
//! These tests assert both halves of the contract for each op:
//!   (1) the new phrasing (e.g. "list destructure") appears in the
//!       rendered error, and
//!   (2) none of the internal opcode names (`DestructList`,
//!       `DestructTuple`, `DestructVariant`, `DestructListRest`,
//!       `DestructRecordField`, `DestructMapValue`) appear.
//!
//! Several error paths — the `on non-<type>` arms — are gated behind the
//! typechecker, which now rejects the programs that would have
//! triggered them before the runtime ran. Those cases are covered by
//! a direct VM unit test that constructs the offending stack state and
//! executes the opcode in isolation; see `test_destruct_*_type_mismatch_*`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

/// Compile and run a script, expecting a runtime error. Returns the
/// fully-rendered error string (including the "error[runtime]: ..."
/// prefix) so callers can assert on both the phrasing and the absence
/// of opcode names.
fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler
        .compile_program(&program)
        .expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

/// Collective set of opcode names that MUST NOT appear in any
/// destruct-related runtime error surfaced to users. Keeping the list
/// here (rather than per-test) makes it a single place to extend if a
/// new destruct op is ever added.
const OPCODE_NAMES: &[&str] = &[
    "DestructList",
    "DestructTuple",
    "DestructVariant",
    "DestructListRest",
    "DestructRecordField",
    "DestructMapValue",
];

fn assert_no_opcode_names(err: &str) {
    for name in OPCODE_NAMES {
        assert!(
            !err.contains(name),
            "error leaked opcode name `{name}`:\n{err}"
        );
    }
}

// ── DestructList index out of bounds ───────────────────────────────

/// User repro from the audit: `let [a, b, c] = [1, 2]` must explain
/// the mismatch in list terms, not in opcode terms.
#[test]
fn test_destruct_list_too_short_reports_user_facing() {
    let err = run_err(
        r#"
fn main() {
  let [a, b, c] = [1, 2]
  println(a)
}
"#,
    );
    assert!(
        err.contains("list destructure"),
        "missing 'list destructure' phrasing: {err}"
    );
    assert!(
        err.contains("expected at least 3") && err.contains("got 2"),
        "missing count detail: {err}"
    );
    assert_no_opcode_names(&err);
}

/// The rest-pattern BindDestructKind::List path (prefix element before
/// the rest binding). Triggers DestructList at index 1 on a 1-element
/// list. Same phrasing contract as the no-rest case.
#[test]
fn test_destruct_list_rest_prefix_too_short_reports_user_facing() {
    let err = run_err(
        r#"
fn main() {
  let [a, b, ..rest] = [1]
  println(a)
}
"#,
    );
    assert!(
        err.contains("list destructure"),
        "missing 'list destructure' phrasing: {err}"
    );
    assert!(
        err.contains("expected at least 2") && err.contains("got 1"),
        "missing count detail: {err}"
    );
    assert_no_opcode_names(&err);
}

// ── DestructVariant / DestructTuple / DestructRecordField etc ──────
//
// The "shape mismatch" (e.g. tuple-destructure on non-tuple) arms are
// unreachable from well-typed source because the typechecker now
// rejects those programs up front. The arity mismatch for tuples is
// also a compile-time error. To still lock in the opcode-name-free
// phrasing for the remaining runtime-reachable arms, we exercise:
//
//   * DestructVariant: a `let` destructure whose bound variant's field
//     count is correct but whose arity-matching depends on the specific
//     variant constructor used — not reachable without a type hole.
//   * DestructMapValue: missing key triggers a different (already
//     user-facing) error, not the type-mismatch arm.
//
// The type-mismatch arms are therefore covered by the direct VM unit
// tests inside `src/vm/execute.rs`'s test module. Here, we additionally
// assert via the compile-time-safe surface that the phrasing holds for
// each runtime-reachable index/length arm.

/// DestructListRest runs with `start > len` when the rest binding
/// lives behind a refutable prefix test that failed to reject short
/// inputs. In current codegen, this is structurally unreachable because
/// the list-length check fires first; but we still want a test that
/// locks the phrasing in place if the codegen ever changes.
///
/// Rather than fabricating an unreachable path, this test asserts the
/// message string via the source of truth: the error text compiled
/// into the VM. Reading the file ensures that if anyone re-introduces
/// the opcode name, this test fails.
#[test]
fn test_destruct_error_strings_do_not_mention_opcode_names_in_source() {
    let source = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/vm/execute.rs"),
    )
    .expect("read execute.rs");

    // Limit scope to the destruct ops' bodies. We can't easily slice by
    // line, but we can require that every line that mentions a
    // destruct-related error phrasing does NOT also mention the opcode
    // name in the same `VmError::new(...)` call.
    //
    // Approach: for each opcode name, ensure it only appears as an
    // `Op::<Name>` match arm or comment, NEVER inside a quoted error
    // string literal.
    for name in OPCODE_NAMES {
        // Quoted string containing the opcode name implies a leaking
        // error message. (The match arm `Op::DestructList =>` contains
        // `DestructList` too, so we have to discriminate on the quote.)
        let needle_quoted = format!("\"{name}");
        assert!(
            !source.contains(&needle_quoted),
            "execute.rs contains an error string starting with opcode name `{name}` \
             — user-facing errors must not leak internal opcode names"
        );
        // Also catch the format-string form: `"...{op}..." with `{op}`
        // substituted. We search for the opcode name followed by a
        // space and a lowercase word that hints at an error phrase
        // (e.g. "DestructList index", "DestructTuple on").
        let needle_idx = format!("{name} index");
        let needle_on = format!("{name} on");
        let needle_start = format!("{name} start");
        assert!(
            !source.contains(&needle_idx),
            "execute.rs still contains `{needle_idx}` — rewrite to user-facing phrasing"
        );
        assert!(
            !source.contains(&needle_on),
            "execute.rs still contains `{needle_on}` — rewrite to user-facing phrasing"
        );
        assert!(
            !source.contains(&needle_start),
            "execute.rs still contains `{needle_start}` — rewrite to user-facing phrasing"
        );
    }
}

// ── LATENT #2: MAX_FRAMES extracted to module scope ────────────────

/// The MAX_FRAMES cap used to be declared twice as a local `const` —
/// once inside `call_value` and once inside `invoke_callable` — with
/// the same literal value. It's now a module-level constant referenced
/// from both sites. This test enforces the single-source-of-truth
/// invariant: exactly one declaration of `const MAX_FRAMES: usize = ...`
/// in execute.rs, and both `call_value` / `invoke_callable` bodies have
/// lost their local `const MAX_FRAMES` declarations.
#[test]
fn test_max_frames_is_single_module_level_constant() {
    let source = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/vm/execute.rs"),
    )
    .expect("read execute.rs");
    let decls: Vec<&str> = source
        .lines()
        .filter(|l| l.contains("const MAX_FRAMES"))
        .collect();
    assert_eq!(
        decls.len(),
        1,
        "expected exactly one `const MAX_FRAMES` declaration, found {}: {:#?}",
        decls.len(),
        decls
    );
    // The surviving declaration must be at module scope (i.e. not
    // indented by a function body). Module-level `const` starts at
    // column 0.
    let decl = decls[0];
    assert!(
        decl.starts_with("const ") || decl.starts_with("pub(crate) const "),
        "MAX_FRAMES declaration is not at module scope: {decl:?}"
    );
}
