#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::lexer::Lexer;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The lexer must never panic — errors are fine.
        let _ = Lexer::new(s).tokenize();
    }
});
