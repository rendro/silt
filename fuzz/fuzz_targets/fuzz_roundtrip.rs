#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_formatter_invariants;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // If the source lexes and parses successfully...
        let file = silt::source::FileId::default();
        let tokens = match Lexer::new(file, s).tokenize() {
            Ok(t) => t,
            Err(_) => return,
        };
        if Parser::new(tokens, s).parse_program().is_err() {
            return;
        }

        // ...then formatting must succeed and the result must still parse.
        let formatted = silt::format::format(file, s).expect("a program is formatted");
        let tokens2 = Lexer::new(file, &formatted)
            .tokenize()
            .expect("Formatted code must lex");
        Parser::new(tokens2, &formatted)
            .parse_program()
            .expect("Formatted code must parse");

        // Not refused, and a second pass changes nothing.
        check_formatter_invariants(s).unwrap_or_else(|err| {
            panic!("Formatter invariant violated: {err}");
        });
    }
});
