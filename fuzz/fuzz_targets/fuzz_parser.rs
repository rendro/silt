#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::lexer::Lexer;
use silt::parser::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(tokens) = Lexer::new(s).tokenize() {
            // The parser must never panic — errors are fine.
            let _ = Parser::new(tokens).parse_program();
        }
    }
});
