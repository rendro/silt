//! Runtime error paths of the VM that the CLI cannot reach.
//!
//! Each program below is rejected by the typechecker (or, for a missing
//! `main`, by the compiler), so `silt run` never gets to the VM. These
//! tests run the VM in process with the typechecker's verdict discarded,
//! to lock the runtime defences that back up the static checks. One more
//! stays here: the `(break)` formatter round trip (uses the formatter
//! API). Every other test of this file is a golden case under
//! `tests/golden/lang/errors/error__*`.

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::vm::Vm;
use std::sync::Arc;

/// Expect a parse error; returns the error message.
/// Uses parse_program which returns the first fatal error.
fn parse_errors(input: &str) -> Vec<String> {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer error");
    match Parser::new(tokens, input).parse_program() {
        Err(e) => vec![e.message.clone()],
        Ok(_) => vec![], // no error; caller should check
    }
}

/// Compile and run, returning the runtime error message.
/// Panics if the program succeeds instead of erroring.
fn run_err(input: &str) -> String {
    let tokens = Lexer::new(silt::source::FileId::default(), input)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens, input)
        .parse_program()
        .expect("parse error");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    match vm.run(script) {
        Err(e) => format!("{e}"),
        Ok(v) => panic!("expected runtime error, got: {v:?}"),
    }
}

#[test]
fn test_runtime_negate_string() {
    let err = run_err(r#"fn main() { -"hello" }"#);
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_negate_bool() {
    let err = run_err("fn main() { -true }");
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_negate_list() {
    let err = run_err("fn main() { -[1, 2, 3] }");
    assert!(err.contains("cannot negate"), "got: {err}");
}

#[test]
fn test_runtime_not_on_int() {
    let err = run_err("fn main() { !42 }");
    // Lock the exact phrase "cannot apply 'not' to Int" so a fallback like
    // "could not..." or an "annotation" error won't accidentally satisfy it.
    assert!(
        err.contains("cannot apply 'not' to Int"),
        "expected \"cannot apply 'not' to Int\", got: {err}"
    );
}

#[test]
fn test_runtime_not_on_string() {
    let err = run_err(r#"fn main() { !"hello" }"#);
    assert!(
        err.contains("cannot apply 'not' to String"),
        "expected \"cannot apply 'not' to String\", got: {err}"
    );
}

#[test]
fn test_runtime_int_float_add() {
    let err = run_err("fn main() { 1 + 2.5 }");
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_runtime_int_float_mul() {
    let err = run_err("fn main() { 3 * 1.5 }");
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_runtime_string_minus_string() {
    let err = run_err(r#"fn main() { "hello" - "world" }"#);
    assert!(err.contains("cannot apply"), "got: {err}");
}

#[test]
fn test_runtime_string_multiply() {
    // Asserts exact VM message from src/vm/arithmetic.rs binary op dispatch
    // ("cannot apply '*' to String and Int").
    let err = run_err(r#"fn main() { "hello" * 3 }"#);
    assert!(
        err.contains("cannot apply '*' to String and Int"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_list_arithmetic() {
    let err = run_err("fn main() { [1, 2] + [3, 4] }");
    assert!(err.contains("cannot apply"), "got: {err}");
}

#[test]
fn test_runtime_cross_type_equality() {
    let err = run_err(r#"fn main() { 42 == "42" }"#);
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_cross_type_comparison() {
    let err = run_err("fn main() { 42 < true }");
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_int_float_equality() {
    let err = run_err("fn main() { 3 == 3.0 }");
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_compare_incompatible_types() {
    let err = run_err(r#"fn main() { "abc" > 123 }"#);
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_runtime_question_mark_on_non_result() {
    // Using ? on something that's not a Result or Option
    let err = run_err(
        r#"
fn foo() {
  let x = 42?
  x
}
fn main() { foo() }
    "#,
    );
    // Lock the actual production string so a trivial fallback that
    // happens to mention "?" (e.g. "parse error: unexpected ?") cannot
    // satisfy this assertion.
    assert!(
        err.contains("`?` applies only to Result or Option") && err.contains("Int"),
        "expected `?` applies-only message mentioning Int, got: {err}"
    );
}

#[test]
fn test_runtime_field_access_on_int() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  x.name
}
    "#,
    );
    // Lock the full phrase "cannot access field 'name' on Int"; a weaker
    // fallback like "unknown field" or a generic "field" substring must
    // not satisfy this.
    assert!(
        err.contains("cannot access field 'name' on Int"),
        "expected \"cannot access field 'name' on Int\", got: {err}"
    );
}

#[test]
fn test_runtime_field_access_on_list() {
    let err = run_err(
        r#"
fn main() {
  let xs = [1, 2, 3]
  xs.name
}
    "#,
    );
    assert!(
        err.contains("cannot access field 'name' on List"),
        "expected \"cannot access field 'name' on List\", got: {err}"
    );
}

#[test]
fn test_runtime_undefined_global() {
    // Asserts exact VM message from src/vm/execute.rs GetGlobal handler
    // ("undefined global: nonexistent_function"). Locks the lowercase
    // spelling so a capitalized fallback elsewhere cannot satisfy it.
    let err = run_err(
        r#"
fn main() { nonexistent_function() }
    "#,
    );
    assert!(
        err.contains("undefined global: nonexistent_function"),
        "got: {err}"
    );
}

#[test]
fn test_program_without_main() {
    // Helper functions but no main — compiles successfully, but at
    // runtime the entry-point script emits `GetGlobal("main")` which
    // fails with "undefined global: main". Locks that exact VM message
    // (same shape as `test_runtime_undefined_global` above) so a
    // silent success or wrong-message regression is caught. The
    // previous body discarded `catch_unwind`'s result, so any behaviour
    // (including the program succeeding) satisfied the test.
    let err = run_err("fn helper(x) { x + 1 }");
    assert!(
        err.contains("undefined global: main"),
        "expected undefined-main runtime error, got: {err}"
    );
}

#[test]
fn test_pipe_into_wrong_arity() {
    // Piping a value into a function that takes 0 args — asserts exact
    // VM message from src/vm/execute.rs function-call arity check
    // ("function 'no_args' expects 0 arguments, got 1").
    let err = run_err(
        r#"
fn no_args() { 42 }
fn main() { 1 |> no_args() }
    "#,
    );
    assert!(
        err.contains("function 'no_args' expects 0 arguments, got 1"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_send_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.send(42, "hello")
}
    "#,
    );
    // Lock the exact production message so that a generic type-mismatch
    // with just the word "expected" cannot satisfy this test.
    assert!(
        err.contains("channel.send requires a channel as first argument"),
        "expected \"channel.send requires a channel as first argument\", got: {err}"
    );
}

#[test]
fn test_runtime_receive_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.receive("not a channel")
}
    "#,
    );
    assert!(
        err.contains("channel.receive requires a channel argument"),
        "expected \"channel.receive requires a channel argument\", got: {err}"
    );
}

#[test]
fn test_runtime_close_non_channel() {
    let err = run_err(
        r#"
import channel
fn main() {
  channel.close(42)
}
    "#,
    );
    assert!(
        err.contains("channel.close requires a channel argument"),
        "expected \"channel.close requires a channel argument\", got: {err}"
    );
}

#[test]
fn test_runtime_channel_send_wrong_arg_count() {
    // Asserts exact builtin arity message from src/builtins/channel.rs
    // ("channel.send takes 2 arguments (channel, value)").
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.send(ch)
}
    "#,
    );
    assert!(
        err.contains("channel.send takes 2 arguments (channel, value)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_task_join_non_handle() {
    let err = run_err(
        r#"
import task
fn main() {
  task.join(42)
}
    "#,
    );
    // Lock the exact production string so a generic "expected X, got Y"
    // type-mismatch error cannot satisfy this assertion.
    assert!(
        err.contains("task.join requires a handle argument"),
        "expected \"task.join requires a handle argument\", got: {err}"
    );
}

#[test]
fn test_runtime_task_cancel_non_handle() {
    let err = run_err(
        r#"
import task
fn main() {
  task.cancel("not a handle")
}
    "#,
    );
    assert!(
        err.contains("task.cancel requires a handle argument"),
        "expected \"task.cancel requires a handle argument\", got: {err}"
    );
}

#[test]
fn test_runtime_task_spawn_non_callable() {
    // Asserts exact builtin message from src/builtins/task.rs
    // ("task.spawn requires a function argument").
    let err = run_err(
        r#"
import task
fn main() {
  task.spawn(42)
}
    "#,
    );
    assert!(
        err.contains("task.spawn requires a function argument"),
        "got: {err}"
    );
}

#[test]
fn test_import_nonexistent_builtin_item() {
    // An import of a name a builtin module does not have is rejected
    // when the program is checked, at the item.
    let source =
        "import list.{ nonexistent_function }\nfn main() { nonexistent_function([1, 2]) }\n";
    let tokens = Lexer::new(silt::source::FileId::default(), source)
        .tokenize()
        .expect("lexer error");
    let mut program = Parser::new(tokens, source)
        .parse_program()
        .expect("parse error");
    let errors: Vec<String> = typechecker::check(&mut program)
        .into_iter()
        .map(|d| d.message)
        .collect();
    assert!(
        errors
            .iter()
            .any(|m| m.contains("module 'list' has no member 'nonexistent_function'")),
        "got: {errors:?}"
    );
}

#[test]
fn test_runtime_call_wrong_arity() {
    // Asserts exact VM message from src/vm/execute.rs function-call arity
    // check ("function 'add' expects 2 arguments, got 3").
    let err = run_err(
        r#"
fn add(a, b) { a + b }
fn main() { add(1, 2, 3) }
    "#,
    );
    assert!(
        err.contains("function 'add' expects 2 arguments, got 3"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_call_non_callable() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  x(1, 2)
}
    "#,
    );
    // Lock the exact production message; the previous OR chain's third
    // branch `callable` subsumed the first two and made the assertion weak.
    assert!(
        err.contains("cannot call value of type Int"),
        "expected \"cannot call value of type Int\", got: {err}"
    );
}

#[test]
fn test_runtime_list_map_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/list.rs
    // ("list.map takes 2 arguments (list, fn)").
    let err = run_err(
        r#"
import list
fn main() { list.map([1, 2]) }
    "#,
    );
    assert!(
        err.contains("list.map takes 2 arguments (list, fn)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_string_split_wrong_type() {
    let err = run_err(
        r#"
import string
fn main() { string.split(42, ",") }
    "#,
    );
    // Round-73 follow-up: canonical "<fn> requires <Kind>, got <kind>" form.
    // Lock substring so the test stays robust against trailing-arg-name variation.
    assert!(
        err.contains("string.split requires String, got Int"),
        "expected \"string.split requires String, got Int\", got: {err}"
    );
}

#[test]
fn test_runtime_map_get_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/map.rs
    // ("map.get takes 2 arguments").
    let err = run_err(
        r#"
import map
fn main() { map.get(#{"a": 1}) }
    "#,
    );
    assert!(err.contains("map.get takes 2 arguments"), "got: {err}");
}

#[test]
fn test_runtime_regex_wrong_arity() {
    // Asserts exact builtin arity message from src/builtins/regex.rs
    // ("regex.is_match takes 2 arguments (pattern, text)").
    let err = run_err(
        r#"
import regex
fn main() { regex.is_match("[a-z]+") }
    "#,
    );
    assert!(
        err.contains("regex.is_match takes 2 arguments (pattern, text)"),
        "got: {err}"
    );
}

#[test]
fn test_runtime_non_exhaustive_match_tuple() {
    let err = run_err(
        r#"
fn main() {
  match (1, 2) {
    (0, 0) -> "origin"
  }
}
    "#,
    );
    // Lock the full panic string so that a type-mismatch or generic
    // "mismatch"/"matched" error cannot satisfy this assertion.
    // This is the most insidious gap: a bug that conflates a type mismatch
    // with a non-exhaustive match would slip through the old `contains("match")`.
    assert!(
        err.contains("non-exhaustive match: no arm matched"),
        "expected \"non-exhaustive match: no arm matched\", got: {err}"
    );
}

#[test]
fn test_runtime_non_exhaustive_match_variant() {
    let err = run_err(
        r#"
fn main() {
  let x = Some(42)
  match x {
    None -> "none"
  }
}
    "#,
    );
    assert!(
        err.contains("non-exhaustive match: no arm matched"),
        "expected \"non-exhaustive match: no arm matched\", got: {err}"
    );
}

#[test]
fn test_parenthesized_break_parses_and_roundtrips_through_formatter() {
    // Regression lock against the formatter/parser roundtrip failure
    // that the removed G1 guard caused. `(break)` is syntactically a
    // paren expression wrapping an ident reference, and the formatter
    // strips redundant parens. The result must still parse — the G1
    // guard used to reject it with a fake "syntax error" even though
    // the token stream is perfectly valid ident-in-statement.
    let src = "fn main() {\n  (break)\n}\n";
    let formatted =
        silt::formatter::format(src).expect("paren-wrapped break must format without error");
    // parse_errors drops any typechecker diagnostics; we only care
    // that the *parser* accepts the formatter's output.
    let perrs = parse_errors(&formatted);
    assert!(
        perrs.is_empty(),
        "parser must accept formatter output for (break); formatted={formatted:?}, errors={perrs:?}"
    );
}
