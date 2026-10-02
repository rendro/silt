//! Regression tests for `list.zip` result-length bounds (L1).
//!
//! Before the fix, `list.zip` computed its result capacity via
//! `ValueIter::len()` which used `size_hint` saturating to `usize::MAX`
//! for huge ranges like `0..i64::MAX`. The subsequent
//! `Vec::with_capacity(usize::MAX)` then panicked opaquely, surfacing
//! as "builtin module 'list' panicked". The fix validates both input
//! lengths (via `checked_range_len` semantics for ranges) against
//! `MAX_RANGE_MATERIALIZE` and returns a clean `VmError` on overflow.
//!
//! The rejection and small-range cases are golden cases in
//! tests/golden/lang/stdlib/list_zip_bounds__*.silt. The at-cap case stays
//! here: materializing 10M tuples takes longer than the golden harness's
//! 20 s per-case limit in a debug build, and in-process it is quicker.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens, input)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

// ── Exactly at the cap: must pass ───────────────────────────────────
//
// `MAX_RANGE_MATERIALIZE` is 10_000_000. The inclusive range `0..9999999`
// yields exactly 10_000_000 elements. Zipping two such ranges must
// produce a 10_000_000-element list without hitting the cap.

#[test]
fn test_list_zip_range_range_at_cap_ok() {
    let result = run(r#"
import list
fn main() {
  list.length(list.zip(0..9999999, 0..9999999))
}
        "#);
    assert_eq!(result, Value::Int(10_000_000));
}
