//! Regression: `time.days_in_month` must reject out-of-range months
//! instead of fabricating a result.
//!
//! BROKEN (pre-fix): `days_in_month`'s helper had a `_ => 30` fallback, so
//! `time.days_in_month(2024, 13)` and `(2024, 0)` silently returned 30.
//! Sibling constructors `time.date`/`time.time` range-validate their
//! components; this one invented a result. The builtin now validates the
//! month is in 1..=12 and raises a runtime error for anything else, while
//! leaving valid-month behavior (incl. Feb leap-year logic) unchanged.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

// ── Runtime helpers (compile + run a silt program) ──────────────────

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── Invalid-month cases: must now produce a runtime error ───────────

#[test]
fn days_in_month_month_13_is_runtime_error() {
    let err = run_err(
        r#"
import time
fn main() -> Int {
  time.days_in_month(2024, 13)
}
"#,
    );
    assert!(
        err.contains("time.days_in_month: month") && err.contains("out of range"),
        "month 13 must be rejected as out of range, got: {err}"
    );
}

#[test]
fn days_in_month_month_0_is_runtime_error() {
    let err = run_err(
        r#"
import time
fn main() -> Int {
  time.days_in_month(2024, 0)
}
"#,
    );
    assert!(
        err.contains("time.days_in_month: month") && err.contains("out of range"),
        "month 0 must be rejected as out of range, got: {err}"
    );
}

// ── Valid-month cases: behavior must be unchanged ───────────────────

fn days_in(year: i64, month: i64) -> i64 {
    let src = format!(
        r#"
import time
fn main() -> Int {{
  time.days_in_month({year}, {month})
}}
"#
    );
    match run(&src) {
        Value::Int(n) => n,
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn days_in_month_valid_months_unchanged() {
    assert_eq!(days_in(2024, 2), 29, "Feb 2024 (leap) = 29");
    assert_eq!(days_in(2023, 2), 28, "Feb 2023 (non-leap) = 28");
    assert_eq!(days_in(2024, 4), 30, "Apr = 30");
    assert_eq!(days_in(2024, 1), 31, "Jan = 31");
    assert_eq!(days_in(2024, 12), 31, "Dec (upper boundary) = 31");
}
