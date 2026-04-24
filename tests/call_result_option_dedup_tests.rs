//! Round 60 L13 lock: `src/builtins/core.rs` must share a single helper
//! for the two-variant ADT dispatch arms that `call_result` and
//! `call_option` use. They previously duplicated ~40 lines of arm
//! template per op (unwrap_or / is_ok+is_some / is_err+is_none /
//! map_ok+map / flat_map). Collapsed into one `dispatch_shared_adt_op`
//! helper per silt's "one way to do things" principle (MEMORY.md).
//!
//! The lock has two parts:
//!   1. Structural: `dispatch_shared_adt_op` is defined exactly once.
//!   2. Behavioral: a tiny silt program exercising `result.*` and
//!      `option.*` observes the same outputs as before the refactor.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

const CORE_SRC: &str = include_str!("../src/builtins/core.rs");

#[test]
fn shared_helper_defined_exactly_once() {
    // Count `fn dispatch_shared_adt_op` definitions. Exactly one means
    // the dedup hasn't been un-done (e.g. by someone copy-pasting the
    // helper body back into `call_result` / `call_option`).
    let occurrences = CORE_SRC.matches("fn dispatch_shared_adt_op").count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one `fn dispatch_shared_adt_op` definition in \
         src/builtins/core.rs (found {}). If this failed because the \
         helper was renamed, update this lock to match.",
        occurrences
    );
    // And both call sites must delegate to it, rather than inlining the
    // shared arms back.
    let callers = CORE_SRC.matches("dispatch_shared_adt_op(").count();
    assert!(
        callers >= 3,
        "expected the shared helper to be called from both `call_result` \
         and `call_option` (the definition counts as one occurrence of \
         the name; each caller adds one), found {} call-site occurrences.",
        callers - 1
    );
}

#[test]
fn call_result_has_no_duplicated_is_ok_arm() {
    // Structural: `"is_ok"` appears only inside the shared helper's
    // argument list in `call_result`, not as a top-level match arm.
    // Before the dedup it appeared as `"is_ok" => { ... }` inside
    // `call_result`. We lock that the string only occurs in a narrow
    // number of places (helper signature + call sites + this test's
    // expected locations).
    let occurrences = CORE_SRC.matches("\"is_ok\"").count();
    // `"is_ok"` should appear exactly once in core.rs (as the string
    // argument passed from `call_result` into `dispatch_shared_adt_op`).
    assert_eq!(
        occurrences, 1,
        "`\"is_ok\"` should appear exactly once in core.rs (as the \
         `is_ok_name` argument in the `call_result` delegation), found \
         {} occurrences — this suggests the dedup was un-done.",
        occurrences
    );
}

// ── Behavioral lock: same observable semantics as pre-dedup ─────────

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

#[test]
fn result_dispatch_behavior_unchanged() {
    // Exercise every op the shared helper handles on the Result side,
    // plus the two module-specific ops (map_err, flatten) that stay
    // inline in `call_result`.
    assert_eq!(
        run("import result\nfn main() { result.unwrap_or(Ok(7), 0) }"),
        Value::Int(7)
    );
    assert_eq!(
        run("import result\nfn main() { result.unwrap_or(Err(\"boom\"), 42) }"),
        Value::Int(42)
    );
    assert_eq!(
        run("import result\nfn main() { result.is_ok(Ok(1)) }"),
        Value::Bool(true)
    );
    assert_eq!(
        run("import result\nfn main() { result.is_ok(Err(\"x\")) }"),
        Value::Bool(false)
    );
    assert_eq!(
        run("import result\nfn main() { result.is_err(Err(\"x\")) }"),
        Value::Bool(true)
    );
    assert_eq!(
        run("import result\nfn main() { result.is_err(Ok(1)) }"),
        Value::Bool(false)
    );
    assert_eq!(
        run("import result\nfn main() { result.map_ok(Ok(3), fn(x) { x * 2 }) }"),
        Value::Variant("Ok".into(), vec![Value::Int(6)])
    );
    assert_eq!(
        run("import result\nfn main() { result.map_ok(Err(\"e\"), fn(x) { x * 2 }) }"),
        Value::Variant("Err".into(), vec![Value::String("e".into())])
    );
    // map_err (result-specific, not in shared helper):
    assert_eq!(
        run("import result\nfn main() { result.map_err(Err(\"boom\"), fn(s) { \"wrapped\" }) }"),
        Value::Variant("Err".into(), vec![Value::String("wrapped".into())])
    );
    // flatten (result-specific):
    assert_eq!(
        run("import result\nfn main() { result.flatten(Ok(Ok(5))) }"),
        Value::Variant("Ok".into(), vec![Value::Int(5)])
    );
    // flat_map (shared helper path):
    assert_eq!(
        run("import result\nfn main() { result.flat_map(Ok(3), fn(x) { Ok(x + 10) }) }"),
        Value::Variant("Ok".into(), vec![Value::Int(13)])
    );
    assert_eq!(
        run("import result\nfn main() { result.flat_map(Err(\"e\"), fn(x) { Ok(x) }) }"),
        Value::Variant("Err".into(), vec![Value::String("e".into())])
    );
}

#[test]
fn option_dispatch_behavior_unchanged() {
    assert_eq!(
        run("import option\nfn main() { option.unwrap_or(Some(7), 0) }"),
        Value::Int(7)
    );
    assert_eq!(
        run("import option\nfn main() { option.unwrap_or(None, 42) }"),
        Value::Int(42)
    );
    assert_eq!(
        run("import option\nfn main() { option.is_some(Some(1)) }"),
        Value::Bool(true)
    );
    assert_eq!(
        run("import option\nfn main() { option.is_some(None) }"),
        Value::Bool(false)
    );
    assert_eq!(
        run("import option\nfn main() { option.is_none(None) }"),
        Value::Bool(true)
    );
    assert_eq!(
        run("import option\nfn main() { option.is_none(Some(1)) }"),
        Value::Bool(false)
    );
    assert_eq!(
        run("import option\nfn main() { option.map(Some(3), fn(x) { x * 2 }) }"),
        Value::Variant("Some".into(), vec![Value::Int(6)])
    );
    assert_eq!(
        run("import option\nfn main() { option.map(None, fn(x) { x * 2 }) }"),
        Value::Variant("None".into(), vec![])
    );
    // flat_map (shared helper path):
    assert_eq!(
        run("import option\nfn main() { option.flat_map(Some(3), fn(x) { Some(x + 10) }) }"),
        Value::Variant("Some".into(), vec![Value::Int(13)])
    );
    assert_eq!(
        run("import option\nfn main() { option.flat_map(None, fn(x) { Some(x) }) }"),
        Value::Variant("None".into(), vec![])
    );
    // to_result (option-specific):
    assert_eq!(
        run("import option\nfn main() { option.to_result(Some(5), \"err\") }"),
        Value::Variant("Ok".into(), vec![Value::Int(5)])
    );
    assert_eq!(
        run("import option\nfn main() { option.to_result(None, \"err\") }"),
        Value::Variant("Err".into(), vec![Value::String("err".into())])
    );
}
