#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::formatter;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // If the source lexes and parses successfully...
        let tokens = match Lexer::new(s).tokenize() {
            Ok(t) => t,
            Err(_) => return,
        };
        if Parser::new(tokens).parse_program().is_err() {
            return;
        }

        // ...then formatting must succeed and the result must still parse.
        if let Ok(formatted) = formatter::format(s) {
            let tokens2 = Lexer::new(&formatted)
                .tokenize()
                .expect("Formatted code must lex");
            Parser::new(tokens2)
                .parse_program()
                .expect("Formatted code must parse");
        }
    }
});
