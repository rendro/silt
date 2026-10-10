#![no_main]
use libfuzzer_sys::fuzz_target;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Cap on the number of diagnostics any single typecheck pass may
/// emit before we treat it as a runaway. Real programs (even the
/// largest stdlib fixtures) sit well under a few hundred; anything
/// approaching this cap indicates a feedback loop where each error
/// triggers further cascade reporting (typically a missing
/// "already-reported" guard around a recursive helper). Catching the
/// runaway here keeps fuzz campaigns from OOMing the host instead of
/// reporting the bug.
const MAX_DIAGNOSTICS: usize = 10_000;

fuzz_target!(|data: &[u8]| {
    // 1. Decode as UTF-8 — typechecker only ever sees text the lexer
    //    accepted, which is by definition valid UTF-8.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // 2. Lex — typechecker only sees token streams that lexed cleanly.
    let Ok(tokens) = Lexer::new(silt::source::FileId::default(), s)
        .tokenize()
        .checked()
    else {
        return;
    };

    // 3. Parse — typechecker entry point requires a `Program`. The
    //    parser's own panic-freedom is the subject of fuzz_parser; here
    //    we skip parse errors so this driver focuses on the
    //    typechecker.
    let Ok(_) = Parser::new(tokens, s).parse_program() else {
        return;
    };

    // 4. Analyse it as every front door does. Must never panic /
    //    unwind on any input that lexed and parsed.
    let errors = silt::session::testing::analyze_str(s).1;

    // 5. Diagnostic count must be bounded — runaway diagnostic
    //    generation indicates a cascade-reporting bug.
    assert!(
        errors.len() <= MAX_DIAGNOSTICS,
        "typechecker produced {} diagnostics (cap {}) — likely cascade bug",
        errors.len(),
        MAX_DIAGNOSTICS
    );

    // 6. Every diagnostic must be well-formed:
    //    a. Non-empty message (an empty string would render as a blank
    //       line in the CLI / LSP and is always a bug).
    //    b. The span must end within the source (`Span::BUILTIN`, for
    //       what silt declares itself, is the empty span at 0). A
    //       diagnostic pointing past EOF would mis-render the caret.
    let src_len = s.len();
    for (idx, err) in errors.iter().enumerate() {
        assert!(
            !err.message.is_empty(),
            "diagnostic #{idx} has empty message: {err:?}"
        );
        assert!(
            err.span.end as usize <= src_len,
            "diagnostic #{idx} span end {} exceeds source length {} \
             (message: {:?})",
            err.span.end,
            src_len,
            err.message
        );
    }
});
