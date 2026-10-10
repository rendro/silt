#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::fuzz_invariants::check_lexer_invariants;
use silt::lexer::Lexer;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The lexer must never panic, and whatever the text, with lex
        // errors or without, structural invariants must hold: monotonic
        // spans, exactly one trailing Eof, no token referencing a byte
        // offset past the end of the source, an error for every Error
        // token.
        let lexed = Lexer::new(silt::source::FileId::default(), s).tokenize();
        check_lexer_invariants(s, &lexed).unwrap_or_else(|err| {
            panic!("Lexer invariant violated: {err}");
        });
    }
});
