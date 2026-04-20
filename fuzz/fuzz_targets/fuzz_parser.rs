#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(tokens) = Lexer::new(s).tokenize() {
            // The parser must never panic — errors are fine.
            let _ = Parser::new(tokens.clone()).parse_program();
            // Recovery path is the LSP's primary consumer (see
            // src/lsp/locals.rs, src/lsp/definitions.rs, src/lsp/ast_walk.rs).
            // Must also be panic-free on arbitrary input.
            let _ = Parser::new(tokens).parse_program_recovering();
        }
    }
});
