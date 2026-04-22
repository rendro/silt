//! Round-36 parity locks for `Vm::binary_arithmetic`.
//!
//! The three ExtFloat arms — `(ExtFloat, ExtFloat)`, `(Float, ExtFloat)`,
//! and `(ExtFloat, Float)` — were collapsed into a single or-pattern arm
//! because they were byte-identical. These tests lock the parity: each
//! arithmetic op (`+ - * / %`) across each of the three pair combinations
//! must produce `Value::ExtFloat(x)` whose `x` matches the independent
//! Rust `f64` reference. If a future `Op` is added to one sub-case but
//! not the others, the or-pattern guarantees matches still handle all
//! three — this test suite catches the divergence.
//!
//! ExtFloat literal form: silt has no `ExtFloat` literal keyword, but
//! `Float / Float` evaluates to `Value::ExtFloat` (the divisor may
//! produce a non-finite result, so the type widens). We use that to
//! construct ExtFloat operands in the test programs.
//!
//! The typechecker permits mixed Float/ExtFloat arithmetic (see
//! `src/typechecker/inference.rs` line 2099-2101 — the Widening Rule),
//! so these programs compile cleanly.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

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

fn assert_ext_float(actual: Value, expected: f64, label: &str) {
    match actual {
        Value::ExtFloat(x) => {
            assert!(
                (x - expected).abs() < 1e-10
                    || (x.is_nan() && expected.is_nan())
                    || (x.is_infinite() && expected.is_infinite() && x.signum() == expected.signum()),
                "{label}: expected ExtFloat({expected}), got ExtFloat({x})"
            );
        }
        other => panic!("{label}: expected Value::ExtFloat, got {other:?}"),
    }
}

// Reference f64 arithmetic independent of the VM — straight Rust std ops.
fn ref_op(op: char, a: f64, b: f64) -> f64 {
    match op {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' => a / b,
        '%' => a % b,
        _ => unreachable!(),
    }
}

// ── (ExtFloat, ExtFloat) arm ─────────────────────────────────────────

#[test]
fn ext_ext_add_parity() {
    // Both operands are `(F / F) -> ExtFloat` products.
    // a = 1.0 / 2.0 = 0.5 (ExtFloat)
    // b = 3.0 / 4.0 = 0.75 (ExtFloat)
    let expected = ref_op('+', 0.5, 0.75);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) + (3.0 / 4.0) }"#),
        expected,
        "(ExtFloat, ExtFloat) Add",
    );
}

#[test]
fn ext_ext_sub_parity() {
    let expected = ref_op('-', 0.5, 0.75);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) - (3.0 / 4.0) }"#),
        expected,
        "(ExtFloat, ExtFloat) Sub",
    );
}

#[test]
fn ext_ext_mul_parity() {
    let expected = ref_op('*', 0.5, 0.75);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) * (3.0 / 4.0) }"#),
        expected,
        "(ExtFloat, ExtFloat) Mul",
    );
}

#[test]
fn ext_ext_div_parity() {
    let expected = ref_op('/', 0.5, 0.75);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) / (3.0 / 4.0) }"#),
        expected,
        "(ExtFloat, ExtFloat) Div",
    );
}

#[test]
fn ext_ext_mod_parity() {
    let expected = ref_op('%', 0.5, 0.75);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) % (3.0 / 4.0) }"#),
        expected,
        "(ExtFloat, ExtFloat) Mod",
    );
}

// ── (ExtFloat, Float) arm — ExtFloat on left, Float on right ────────

#[test]
fn ext_flt_add_parity() {
    let expected = ref_op('+', 0.5, 1.0);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) + 1.0 }"#),
        expected,
        "(ExtFloat, Float) Add",
    );
}

#[test]
fn ext_flt_sub_parity() {
    let expected = ref_op('-', 0.5, 1.0);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) - 1.0 }"#),
        expected,
        "(ExtFloat, Float) Sub",
    );
}

#[test]
fn ext_flt_mul_parity() {
    let expected = ref_op('*', 0.5, 2.0);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) * 2.0 }"#),
        expected,
        "(ExtFloat, Float) Mul",
    );
}

#[test]
fn ext_flt_div_parity() {
    let expected = ref_op('/', 0.5, 2.0);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) / 2.0 }"#),
        expected,
        "(ExtFloat, Float) Div",
    );
}

#[test]
fn ext_flt_mod_parity() {
    let expected = ref_op('%', 0.5, 0.3);
    assert_ext_float(
        run(r#"fn main() { (1.0 / 2.0) % 0.3 }"#),
        expected,
        "(ExtFloat, Float) Mod",
    );
}

// ── (Float, ExtFloat) arm — Float on left, ExtFloat on right ────────

#[test]
fn flt_ext_add_parity() {
    let expected = ref_op('+', 1.0, 0.5);
    assert_ext_float(
        run(r#"fn main() { 1.0 + (1.0 / 2.0) }"#),
        expected,
        "(Float, ExtFloat) Add",
    );
}

#[test]
fn flt_ext_sub_parity() {
    let expected = ref_op('-', 1.0, 0.5);
    assert_ext_float(
        run(r#"fn main() { 1.0 - (1.0 / 2.0) }"#),
        expected,
        "(Float, ExtFloat) Sub",
    );
}

#[test]
fn flt_ext_mul_parity() {
    let expected = ref_op('*', 2.0, 0.5);
    assert_ext_float(
        run(r#"fn main() { 2.0 * (1.0 / 2.0) }"#),
        expected,
        "(Float, ExtFloat) Mul",
    );
}

#[test]
fn flt_ext_div_parity() {
    let expected = ref_op('/', 2.0, 0.5);
    assert_ext_float(
        run(r#"fn main() { 2.0 / (1.0 / 2.0) }"#),
        expected,
        "(Float, ExtFloat) Div",
    );
}

#[test]
fn flt_ext_mod_parity() {
    let expected = ref_op('%', 2.0, 0.3);
    assert_ext_float(
        run(r#"fn main() { 2.0 % (3.0 / 10.0) }"#),
        expected,
        "(Float, ExtFloat) Mod",
    );
}

// ── value_disc stability lock ───────────────────────────────────────
//
// `value_disc` was renumbered to close the discriminant-2 gap left by a
// removed variant. The function is only used for equality checks in
// `check_same_type`, never as `Ord` for persisted data, so renumbering
// is a no-op semantically. Still, lock the invariants that matter:
// distinct Silt types must have distinct discriminants, and the
// Float/ExtFloat pair (permitted by the typechecker as mixed operands)
// must share a discriminant so `check_same_type` accepts that mix.

#[test]
fn float_extfloat_share_discriminant_for_eq() {
    // If this test fails, the VM will start rejecting `float == extfloat`
    // comparisons that the typechecker accepts — a silent runtime
    // regression vs the type-level contract.
    //
    // Drive through silt source: equality across Float and ExtFloat
    // must yield Bool, not a runtime type-mismatch error.
    let result = run(r#"fn main() { 1.0 == (2.0 / 2.0) }"#);
    assert_eq!(
        result,
        Value::Bool(true),
        "Float == ExtFloat must succeed with language_eq semantics (widened f64 compare)"
    );
    let result_neq = run(r#"fn main() { 1.0 != (1.0 / 2.0) }"#);
    assert_eq!(
        result_neq,
        Value::Bool(true),
        "Float != ExtFloat must succeed",
    );
}

#[test]
fn int_float_disc_differ_rejects_mixed_eq() {
    // Locks that distinct-disc types still reject mixed equality at the
    // VM layer (the typechecker may have already rejected this, but the
    // VM is the last line of defence).
    //
    // A program that mixes Int and Float for `==` should be rejected.
    // We use the typechecker-permissive `run` (which ignores type
    // errors) and expect a VM runtime error, caught via expect_err.
    let input = r#"fn main() { 1 == 1.0 }"#;
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    // Compile may fail with a type error — that's also acceptable; we
    // just want to confirm the program does NOT produce a successful
    // `Value::Bool(_)` out of the VM. Either rejection path counts.
    let compile_result = compiler.compile_program(&program);
    match compile_result {
        Err(_) => {
            // Compile-time rejection — acceptable.
        }
        Ok(functions) => {
            let script = Arc::new(functions.into_iter().next().unwrap());
            let mut vm = Vm::new();
            let run_result = vm.run(script);
            assert!(
                run_result.is_err(),
                "Int == Float must be rejected somewhere in the pipeline"
            );
        }
    }
}
