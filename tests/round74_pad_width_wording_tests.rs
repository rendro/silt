//! Round 74 — `string.pad_left` / `string.pad_right` say "negative width"
//!
//! Round 73 audit — BROKEN: the third positional argument to
//! `string.pad_{left,right}` is `width`, not an index. The error
//! site previously emitted "negative index {n}" — verbatim copy-paste
//! from a sibling `negative-index` rejection — even though the
//! rejected value is a width parameter. Round 74 renames the wording
//! to "negative width {n}" at both sites.
//!
//! Lock both shapes:
//!   1. SOURCE-GREP: `src/builtins/string.rs` no longer contains the
//!      bug substring `"negative index"` in either `pad_left` or
//!      `pad_right` block.
//!   2. BEHAVIORAL: invoking `string.pad_left("hi", -1, " ")` (and
//!      similarly `pad_right`) emits an error containing
//!      "negative width" (NOT "negative index").

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

const STRING_RS: &str = include_str!("../src/builtins/string.rs");

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── SOURCE-GREP locks ───────────────────────────────────────────────

/// `string.pad_left` must say "negative width", not "negative index".
/// The parameter is a width, not an index — the old wording was a
/// copy-paste leak. Lock against the bug substring at the pad sites.
#[test]
fn string_pad_left_uses_negative_width_not_negative_index() {
    // The pad_left site must contain the canonical "negative width"
    // wording.
    assert!(
        STRING_RS.contains("string.pad_left: negative width"),
        "src/builtins/string.rs missing canonical \
         'string.pad_left: negative width' wording — the third \
         positional is `width`, not an index"
    );
    // And must NOT contain the old "negative index" wording for
    // pad_left.
    assert!(
        !STRING_RS.contains("string.pad_left: negative index"),
        "src/builtins/string.rs still emits \
         'string.pad_left: negative index' — the parameter is a \
         width, not an index. Use 'negative width' instead."
    );
}

#[test]
fn string_pad_right_uses_negative_width_not_negative_index() {
    assert!(
        STRING_RS.contains("string.pad_right: negative width"),
        "src/builtins/string.rs missing canonical \
         'string.pad_right: negative width' wording — the third \
         positional is `width`, not an index"
    );
    assert!(
        !STRING_RS.contains("string.pad_right: negative index"),
        "src/builtins/string.rs still emits \
         'string.pad_right: negative index' — the parameter is a \
         width, not an index. Use 'negative width' instead."
    );
}

// ── BEHAVIORAL locks ────────────────────────────────────────────────

#[test]
fn string_pad_left_negative_width_runtime_error_says_negative_width() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_left("hi", -1, " ")
}
"#,
    );
    assert!(
        err.contains("negative width"),
        "expected pad_left error to mention 'negative width', got: {err}"
    );
    assert!(
        !err.contains("negative index"),
        "pad_left error must NOT say 'negative index' — the \
         third positional is a width, not an index. Got: {err}"
    );
}

#[test]
fn string_pad_right_negative_width_runtime_error_says_negative_width() {
    let err = run_err(
        r#"
import string
fn main() {
  string.pad_right("hi", -1, " ")
}
"#,
    );
    assert!(
        err.contains("negative width"),
        "expected pad_right error to mention 'negative width', got: {err}"
    );
    assert!(
        !err.contains("negative index"),
        "pad_right error must NOT say 'negative index' — the \
         third positional is a width, not an index. Got: {err}"
    );
}
