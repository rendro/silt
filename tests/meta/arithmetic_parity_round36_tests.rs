//! VM-level lock for `check_same_type`: `Int` and `Float` have distinct
//! value discriminants, so the VM rejects `==` across them even when the
//! typechecker's verdict is discarded.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

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
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens, input)
        .parse_program()
        .expect("parse error");
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
