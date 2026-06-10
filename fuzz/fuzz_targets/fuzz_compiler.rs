#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::compiler::Compiler;
use silt::disassemble::disassemble_function;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::types::Severity;

// Compiler-stage fuzz target (the stage between the typechecker and
// the VM). The round-92 string-interpolation bug — >255 segments
// overflowed the u8 `Op::StringConcat` operand, panicking in debug and
// silently emitting wrong bytecode in release — lived exactly here and
// was invisible to every earlier target (lexer/parser/formatter/
// roundtrip/typechecker all stop before bytecode emission).
//
// Pipeline mirrors `src/cli/pipeline.rs`: lex → parse → typecheck →
// compile. We only compile programs whose typecheck is fully clean
// (no `Severity::Error` diagnostics; warnings are permitted, as in
// production) so this driver focuses on the compiler: ill-typed
// inputs are the typechecker target's domain, and clean-program seeds
// are what reach deep emission paths. NOTE: the production pipeline
// actually compiles even in the presence of type errors (module
// resolution happens at compile time), so the compiler must never
// panic there either — widening this gate is a possible follow-up,
// kept narrow for now per the round-93 wiring.
//
// We deliberately do NOT execute the compiled program in the VM:
// arbitrary fuzzed programs may not terminate, and the only stepping
// primitive (`Vm::execute_slice`) is a scheduler slicing API, not a
// sandbox — builtins can perform real IO. Compile-only is the correct
// scope for this target.

fuzz_target!(|data: &[u8]| {
    // 1. Decode as UTF-8 — the compiler only ever sees text the lexer
    //    accepted, which is by definition valid UTF-8.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // 2. Lex — skip inputs that don't lex; lexer panic-freedom is
    //    fuzz_lexer's subject.
    let Ok(tokens) = Lexer::new(s).tokenize() else {
        return;
    };

    // 3. Parse — compiler entry point requires a `Program`; parser
    //    panic-freedom is fuzz_parser's subject.
    let Ok(mut program) = Parser::new(tokens).parse_program() else {
        return;
    };

    // 4. Typecheck. Panic-freedom and diagnostic well-formedness here
    //    are fuzz_typechecker's subject; this driver only uses the
    //    result as a gate. Run before compile (same order as
    //    production — the checker annotates the AST in place).
    let type_errors = typechecker::check(&mut program);
    if type_errors
        .iter()
        .any(|e| e.severity == Severity::Error)
    {
        return;
    }

    // 5. Compile. Must never panic / unwind on any program that lexed,
    //    parsed, and typechecked cleanly. A `CompileError` result is
    //    fine — that's the compiler doing its job (e.g. the round-92
    //    "string interpolation has N segments; limited to 255" guard,
    //    or "no project root set" for user-module imports, which this
    //    rootless Compiler cannot resolve).
    let mut compiler = Compiler::new();
    let Ok(functions) = compiler.compile_program(&program) else {
        return;
    };

    // 6. A successful compile must produce at least one function — the
    //    CLI treats an empty result as an internal error
    //    (`src/cli/pipeline.rs::compile_file_with_options`).
    assert!(
        !functions.is_empty(),
        "compile_program returned Ok but produced no functions"
    );

    // 7. Compiler warnings must be well-formed: a warning with an
    //    empty message renders as a blank diagnostic line and is
    //    always a bug.
    for (idx, w) in compiler.warnings().iter().enumerate() {
        assert!(
            !w.message.is_empty(),
            "compile warning #{idx} has empty message (span {:?})",
            w.span
        );
    }

    // 8. Disassembly must round-trip over every emitted function
    //    without panicking: `disassemble_function` decodes every
    //    instruction and its operands (recursing into nested
    //    `VmClosure` constants), so truncated or malformed operand
    //    encodings — the round-92 bug class — are caught here without
    //    ever executing the program.
    for func in &functions {
        let text = disassemble_function(func);
        assert!(
            !text.is_empty(),
            "disassembly of function {:?} produced empty output",
            func.name
        );
    }
});
