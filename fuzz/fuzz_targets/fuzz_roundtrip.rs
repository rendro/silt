#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::formatter;
use silt::fuzz_invariants::{check_format_idempotent, check_formatter_invariants};
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

            // Structural invariants: token count (minus whitespace),
            // delimiter balance, comment-marker count, and parse-
            // preservation must all be upheld between input and output.
            check_formatter_invariants(s, &formatted).unwrap_or_else(|err| {
                panic!("Formatter invariant violated: {err}");
            });

            // Idempotency: a second pass must produce the same output.
            check_format_idempotent(s).unwrap_or_else(|err| {
                panic!("Formatter idempotency violated: {err}");
            });
        }
    }
});
