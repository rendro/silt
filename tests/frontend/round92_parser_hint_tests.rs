//! Round 92 — parser diagnostic fixes. The findings and their
//! statement-position controls live in golden cases
//! (tests/golden/frontend/hints/round92_parser_hint__*.silt); what stays
//! here is the example-corpus parse control.

use silt::lexer::Lexer;
use silt::parser::Parser;

/// Parse a source string with the strict entry point; Ok(()) when the
/// whole program parses cleanly.
fn parse_ok(input: &str) -> Result<(), String> {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .map_err(|e| format!("{e:?}"))?;
    Parser::new(tokens, input)
        .parse_program()
        .map(|_| ())
        .map_err(|e| e.message)
}

// ────────────────────────────────────────────────────────────────────
// Controls: accepted programs stay byte-identical
// ────────────────────────────────────────────────────────────────────

/// Broader control: real example programs still parse cleanly through
/// the same public API.
#[test]
fn example_programs_still_parse() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for name in ["birthdays.silt", "calculator.silt", "budget.silt"] {
        let path = manifest.join("examples").join(name);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        parse_ok(&src).unwrap_or_else(|e| panic!("examples/{name} must keep parsing: {e}"));
    }
}
