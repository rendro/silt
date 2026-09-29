//! Lock tests for the `when Pattern = expr` -> `when let Pattern = expr`
//! syntax change (commit f9d6446).
//!
//! Commit f9d6446 changed refutable-pattern `when` from:
//!
//!     when Some(x) = Some(42) else { return }
//!
//! to:
//!
//!     when let Some(x) = Some(42) else { return }
//!
//! All existing tests were updated to the new syntax, but no test
//! verified that the OLD syntax is rejected. These tests lock that
//! rejection so a parser revert cannot silently re-enable both forms.

use silt::lexer::Lexer;
use silt::parser::Parser;

/// Attempt to parse `src` and return `Ok(())` on success or `Err(msg)`
/// on parse error.
fn try_parse(src: &str) -> Result<(), String> {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    match Parser::new(tokens).parse_program() {
        Ok(_) => Ok(()),
        Err(e) => Err(e.message.clone()),
    }
}

/// The old `when Pattern = expr` form (without `let`) must be rejected
/// by the parser. Before f9d6446 this was accepted; after the change
/// the parser expects `else` immediately after the pattern and chokes
/// on the `=`.
#[test]
fn test_old_when_pattern_syntax_rejected() {
    let src = r#"
fn main() {
  when Some(x) = Some(42) else { return }
}
"#;
    let result = try_parse(src);
    assert!(
        result.is_err(),
        "old `when Pattern = expr` syntax should be rejected, but parsed successfully"
    );
    let msg = result.unwrap_err();
    assert!(
        msg.contains("expected else, found ="),
        "error message should mention the unexpected `=`; got: {msg}"
    );
}

/// The new `when let Pattern = expr` form must be accepted.
#[test]
fn test_new_when_let_syntax_accepted() {
    let src = r#"
fn main() {
  when let Some(x) = Some(42) else { return }
}
"#;
    let result = try_parse(src);
    assert!(
        result.is_ok(),
        "new `when let Pattern = expr` syntax should be accepted, but got error: {}",
        result.unwrap_err()
    );
}

/// Boolean `when` (no pattern, just a condition) was unchanged by the
/// syntax migration and must still work.
#[test]
fn test_old_when_bool_syntax_still_works() {
    let src = r#"
fn main() {
  when true else { return }
}
"#;
    let result = try_parse(src);
    assert!(
        result.is_ok(),
        "`when <bool> else {{ ... }}` should still be accepted, but got error: {}",
        result.unwrap_err()
    );
}
