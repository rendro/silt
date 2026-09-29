//! Round 17 F21 lock: `warn_if_shadows_module` format string had an
//! unterminated single quote.
//!
//! The format string in `src/compiler/mod.rs:2420-2421` previously
//! read:
//!
//! ```ignore
//! "variable '{s}' shadows the builtin '{s}' module; \
//!  use a different name to access '{s}.* functions"
//! ```
//!
//! Note the `'{s}.*` has no closing `'` before ` functions`. The
//! resulting warning text was:
//!
//!     variable 'result' shadows the builtin 'result' module; use a
//!     different name to access 'result.* functions
//!
//! which reads as if `result.* functions` were a single quoted token
//! — an unbalanced pair of apostrophes that confuses grep-for-quotes
//! users and makes copy/paste of the suggested fix awkward.
//!
//! The fix closes the quote: `'{s}.*' functions`. This test asserts:
//!
//!   1. The compiler emits a shadow-warning for a variable named
//!      after a builtin module (`result`).
//!   2. The warning message contains the exact closed-quote
//!      substring `'result.*' functions`.
//!   3. The total number of single-quote characters (`'`) in the
//!      warning message is even — a structural check that catches
//!      any future regression where a stray `'` gets added or
//!      removed. Before the fix this count was 5 (odd); after it's
//!      6 (even).

#![allow(clippy::mutable_key_type)]

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Compile a program that shadows the `result` builtin module, and
/// return all warning messages emitted by the compiler.
fn compile_and_collect_warnings(src: &str) -> Vec<String> {
    let tokens = Lexer::new(src).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let _ = compiler.compile_program(&program).expect("compile error");
    compiler
        .warnings()
        .iter()
        .map(|w| w.message.clone())
        .collect()
}

/// F21 primary lock: the `'result.*' functions` substring appears
/// with a CLOSING `'` between `.*` and ` functions`.
#[test]
fn test_shadow_warning_has_balanced_quotes() {
    // A variable named `result` shadows the `result` builtin module.
    // (Using `let result = ...` is the canonical trigger.)
    let warnings = compile_and_collect_warnings(
        r#"
fn main() -> Int {
  let result = 42
  result
}
"#,
    );

    let shadow_warnings: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains("shadows the builtin"))
        .collect();
    assert!(
        !shadow_warnings.is_empty(),
        "expected a shadow-warning for 'result', got warnings: {warnings:?}"
    );

    let msg = shadow_warnings[0];

    // Primary assertion: the closed-quote substring is present.
    // Before the fix this was `'result.* functions` (no closing `'`).
    assert!(
        msg.contains("'result.*' functions"),
        "shadow warning missing closed-quote `'result.*' functions`; \
         got: {msg}"
    );

    // Defense-in-depth: the total number of `'` characters must be
    // even. The buggy message had 5 single quotes:
    //   'result'  'result'  'result.*   → 5 apostrophes, odd.
    // The fixed message has 6:
    //   'result'  'result'  'result.*'  → 6 apostrophes, even.
    let quote_count = msg.chars().filter(|c| *c == '\'').count();
    assert!(
        quote_count % 2 == 0,
        "shadow warning has odd single-quote count ({quote_count}) — \
         an unbalanced quote regressed into the format string; got: {msg}"
    );

    // Belt-and-suspenders: explicitly reject the buggy exact
    // substring. If anyone re-introduces the unterminated quote,
    // this substring will come back.
    assert!(
        !msg.contains("'result.* functions"),
        "shadow warning contains the buggy unterminated-quote \
         substring `'result.* functions` (missing closing `'`); \
         got: {msg}"
    );
}

/// Companion test: the shadow warning should NOT suggest the user
/// can still access `result.*` functions by the normal route — the
/// point of the warning is that shadowing disables module access.
/// This isn't the F21 fix itself but it locks the warning's
/// content shape so a future rewrite can't accidentally invert
/// the meaning.
#[test]
fn test_shadow_warning_mentions_module_and_variable() {
    let warnings = compile_and_collect_warnings(
        r#"
fn main() -> Int {
  let result = 42
  result
}
"#,
    );

    let shadow_msg = warnings
        .iter()
        .find(|w| w.contains("shadows the builtin"))
        .expect("expected a shadow warning");

    // Must name the variable, the module, and mention "shadows".
    assert!(
        shadow_msg.contains("'result'"),
        "warning should quote the variable name, got: {shadow_msg}"
    );
    assert!(
        shadow_msg.contains("module"),
        "warning should mention 'module', got: {shadow_msg}"
    );
    assert!(
        shadow_msg.contains("shadows"),
        "warning should use the verb 'shadows', got: {shadow_msg}"
    );
}
