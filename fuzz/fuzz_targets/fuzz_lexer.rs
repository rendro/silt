#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_lexer_invariants;
use silt::lexer::Lexer;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The lexer must never panic — errors are fine.
        if let Ok(tokens) = Lexer::new(s).tokenize() {
            // If tokenization succeeds, structural invariants must hold:
            // monotonic spans, exactly one trailing Eof, and no token
            // referencing a byte offset past the end of the source.
            check_lexer_invariants(s, &tokens).unwrap_or_else(|err| {
                panic!("Lexer invariant violated: {err}");
            });
        }
    }
});
