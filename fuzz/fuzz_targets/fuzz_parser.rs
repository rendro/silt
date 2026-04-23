#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_parser_invariants;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(tokens) = Lexer::new(s).tokenize() {
            // The parser must never panic — errors are fine.
            if let Ok(program) = Parser::new(tokens.clone()).parse_program() {
                // If parsing succeeds, structural invariants on the AST
                // must hold: no span past the source end, non-empty decl
                // list for non-trivial source, and decl count bounded by
                // token count. Catches silent AST-corruption bugs that
                // the old panic-only driver missed.
                check_parser_invariants(s, &tokens, &program).unwrap_or_else(|err| {
                    panic!("Parser invariant violated: {err}");
                });
            }
            // Recovery path is the LSP's primary consumer (see
            // src/lsp/locals.rs, src/lsp/definitions.rs, src/lsp/ast_walk.rs).
            // Must also be panic-free on arbitrary input.
            let _ = Parser::new(tokens).parse_program_recovering();
        }
    }
});
