//! Round 74 — `list.take` / `list.drop` say "negative count"
//!
//! Round 73 audit — BROKEN: the second positional argument to
//! `list.take` and `list.drop` is a `count`, not an index. The
//! sites previously emitted "negative index {n}" — verbatim
//! copy-paste from sibling list-index rejections — even though
//! the rejected value is a count parameter. Compare to
//! `string.repeat` (`src/builtins/string.rs:181`) which correctly
//! says "negative count {n}".
//!
//! Lock both shapes:
//!   1. SOURCE-GREP: `src/builtins/collections.rs` no longer
//!      contains the bug substring `"list.take: negative index"`
//!      / `"list.drop: negative index"`.
//!   2. BEHAVIORAL: invoking `list.take([1,2,3], -1)` emits an
//!      error containing "negative count" (NOT "negative index").

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::vm::Vm;
use std::sync::Arc;

const COLLECTIONS_RS: &str = include_str!("../src/builtins/collections.rs");

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

#[test]
fn list_take_uses_negative_count_not_negative_index() {
    assert!(
        COLLECTIONS_RS.contains("list.take: negative count"),
        "src/builtins/collections.rs missing canonical \
         'list.take: negative count' wording — the second \
         positional is a `count`, not an index"
    );
    assert!(
        !COLLECTIONS_RS.contains("list.take: negative index"),
        "src/builtins/collections.rs still emits \
         'list.take: negative index' — the parameter is a count, \
         not an index. Use 'negative count' instead (compare \
         string.repeat: 'negative count')."
    );
}

#[test]
fn list_drop_uses_negative_count_not_negative_index() {
    assert!(
        COLLECTIONS_RS.contains("list.drop: negative count"),
        "src/builtins/collections.rs missing canonical \
         'list.drop: negative count' wording — the second \
         positional is a `count`, not an index"
    );
    assert!(
        !COLLECTIONS_RS.contains("list.drop: negative index"),
        "src/builtins/collections.rs still emits \
         'list.drop: negative index' — the parameter is a count, \
         not an index. Use 'negative count' instead (compare \
         string.repeat: 'negative count')."
    );
}

// ── BEHAVIORAL locks ────────────────────────────────────────────────

#[test]
fn list_take_negative_count_runtime_error_says_negative_count() {
    let err = run_err(
        r#"
import list
fn main() { list.take([1, 2, 3], -1) }
"#,
    );
    assert!(
        err.contains("negative count"),
        "expected list.take error to mention 'negative count', got: {err}"
    );
    assert!(
        !err.contains("negative index"),
        "list.take error must NOT say 'negative index' — the \
         second positional is a count, not an index. Got: {err}"
    );
}

#[test]
fn list_drop_negative_count_runtime_error_says_negative_count() {
    let err = run_err(
        r#"
import list
fn main() { list.drop([1, 2, 3], -1) }
"#,
    );
    assert!(
        err.contains("negative count"),
        "expected list.drop error to mention 'negative count', got: {err}"
    );
    assert!(
        !err.contains("negative index"),
        "list.drop error must NOT say 'negative index' — the \
         second positional is a count, not an index. Got: {err}"
    );
}
