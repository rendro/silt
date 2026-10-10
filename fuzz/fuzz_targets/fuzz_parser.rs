#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_parser_invariants;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The parser must never panic, whatever the tokens (the lexer's
        // Error tokens among them): it is the language server's parser
        // too, and reads every text that is being typed.
        let lexed = Lexer::new(silt::source::FileId::default(), s).tokenize();
        let (program, errors) = Parser::new(lexed.clone(), s).parse_program_recovering();
        if errors.is_empty() {
            // If parsing succeeds, structural invariants on the AST
            // must hold: no span past the source end, non-empty decl
            // list for non-trivial source, and decl count bounded by
            // token count. Catches silent AST-corruption bugs that
            // the old panic-only driver missed.
            check_parser_invariants(s, &lexed, &program).unwrap_or_else(|err| {
                panic!("Parser invariant violated: {err}");
            });
        }
    }
});
