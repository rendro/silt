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
//! The typechecker rejects a refutable list pattern in `let`, so these
//! programs only reach the VM when run in process with the typechecker's
//! verdict discarded; that is why they stay in Rust.
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
    let functions = compiler.compile_program(&program).expect("compile error");
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
