#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::disassemble::disassemble_function;
use silt::lexer::Lexer;
use silt::parser::Parser;

// Compiler-stage fuzz target (the stage between the typechecker and
// the VM). The round-92 string-interpolation bug — >255 segments
// overflowed the u8 `Op::StringConcat` operand, panicking in debug and
// silently emitting wrong bytecode in release — lived exactly here and
// was invisible to every earlier target (lexer/parser/formatter/
// roundtrip/typechecker all stop before bytecode emission).
//
// Pipeline: the session's analysis, then the compiler, as every front
// door runs them; only programs whose analysis has no error are
// compiled.
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
    let Ok(tokens) = Lexer::new(silt::source::FileId::default(), s)
        .tokenize()
        .checked()
    else {
        return;
    };

    // 3. Parse — parser panic-freedom is fuzz_parser's subject.
    if Parser::new(tokens, s).parse_program().is_err() {
        return;
    }

    // 4. Analyse and compile as every front door does. Ill-typed
    //    inputs are the typechecker target's domain: a program with an
    //    error is not compiled. A compile error is fine (the
    //    "string interpolation has N segments; limited to 255" guard);
    //    a panic is not.
    let Ok(program) = silt::session::testing::compile_decls_str(s) else {
        return;
    };
    let functions = &program.functions;

    // 5. A successful compile must produce at least one function: the
    //    script.
    assert!(
        !functions.is_empty(),
        "compile returned Ok but produced no functions"
    );

    // 6. Disassembly must round-trip over every emitted function
    //    without panicking: `disassemble_function` decodes every
    //    instruction and its operands (recursing into nested
    //    `VmClosure` constants), so truncated or malformed operand
    //    encodings — the round-92 bug class — are caught here without
    //    ever executing the program.
    for func in functions {
        let text = disassemble_function(func, &program.globals);
        assert!(
            !text.is_empty(),
            "disassembly of function {:?} produced empty output",
            func.name()
        );
    }
});
