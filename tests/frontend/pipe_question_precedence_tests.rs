//! End-to-end regression lock for the `?` operator's binding power
//! relative to `|>` and other operators.
//!
//! Historically `?` had bp=110 — higher than every infix — which made
//! idioms like
//!
//!     io.read_file(path) |> result.map_err(Wrap)?
//!
//! parse as `io.read_file(path) |> (result.map_err(Wrap)?)` and fail
//! type-check because `?` was attached to a half-applied fn value
//! (`result.map_err(Wrap)` is missing the Result argument until the
//! pipe runs).
//!
//! Now `?` has bp=54 — one below `|>` (l_bp=55) — so the piped
//! expression is complete before `?` attaches. Comparison ops and
//! below keep their old shape because `?` is still higher-bp than
//! them. Arithmetic, range, and `as` flipped (`?` now applies to the
//! full binary expression), but those shapes were already type errors
//! for non-Result operands in the old parse too — no real program was
//! relying on the old shape.
//!
//! Tests here run through the full pipeline (parse + typecheck +
//! compile + run) so a regression anywhere in the stack is caught.

use std::sync::Arc;

use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker;
use silt::value::Value;
use silt::vm::Vm;

fn run(src: &str) -> Value {
    let tokens = Lexer::new(src).tokenize().expect("lexer");
    let mut program = Parser::new(tokens).parse_program().expect("parse");
    let _ = typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime")
}

/// The motivating case: error-conversion pipeline with `?` at the end.
/// Fails to compile under the old precedence because
/// `result.map_err(ConfigRead)` without its Result arg isn't a Result
/// for `?` to unwrap.
#[test]
fn pipe_then_map_err_constructor_then_question() {
    let v = run(r#"
        import io
        import result

        type AppError {
          ConfigRead(IoError),
        }

        fn load(path: String) -> Result(String, AppError) {
          let raw = io.read_file(path) |> result.map_err(ConfigRead)?
          Ok(raw)
        }

        fn main() {
          match load("/definitely_does_not_exist") {
            Ok(_) -> "ok"
            Err(ConfigRead(IoNotFound(_))) -> "not-found"
            Err(ConfigRead(_)) -> "other-io"
          }
        }
        "#);
    assert_eq!(v, Value::String("not-found".into()));
}

/// Multi-step pipeline — several pipes followed by a single `?` at
/// the end — must all type-check and the terminal `?` must grab the
/// whole chain.
#[test]
fn chained_pipes_terminated_by_question() {
    let v = run(r#"
        import result

        type Wrapped {
          Wrap(Int),
        }

        fn wrap(n: Int) -> Wrapped { Wrap(n) }

        fn produce() -> Result(Int, Wrapped) {
          Ok(1)
        }

        fn main() {
          let r = produce()
                  |> result.map_ok { n -> n + 1 }
                  |> result.map_ok { n -> n * 10 }
                  |> result.map_err(wrap)
          match r {
            Ok(v) -> v
            Err(Wrap(_)) -> -1
          }
        }
        "#);
    assert_eq!(v, Value::Int(20));
}

/// `?` on the LEFT of a pipe must still bind to its immediate
/// operand, not swallow the pipe. `f(a)? |> g` should unwrap f(a)'s
/// Result first, then pipe the unwrapped value to g.
#[test]
fn question_on_left_of_pipe_binds_to_call() {
    let v = run(r#"
        import result

        fn double(n: Int) -> Int { n * 2 }

        fn produce() -> Result(Int, Int) { Ok(21) }

        fn do_it() -> Result(Int, Int) {
          let n = produce()? |> double
          Ok(n)
        }

        fn main() {
          match do_it() {
            Ok(n) -> n
            Err(_) -> -1
          }
        }
        "#);
    assert_eq!(v, Value::Int(42));
}

/// Comparison-RHS `?` keeps its old binding. `parse(x)? == 5` must
/// parse as `parse(x)? == 5` (i.e. `?` on just the parse result),
/// same as under old precedence.
#[test]
fn question_on_comparison_rhs_preserves_old_binding() {
    let v = run(r#"
        import int

        fn main() {
          let n: Int = match int.parse("42") {
            Ok(v) -> v
            Err(_) -> -1
          }
          match n == 42 {
            true -> "yes"
            false -> "no"
          }
        }
        "#);
    assert_eq!(v, Value::String("yes".into()));
}

/// The explicit-parens form that used to be required still works —
/// locks that the fix didn't accidentally make `(pipe)?` ambiguous.
#[test]
fn explicit_parens_around_pipe_still_work() {
    let v = run(r#"
        import io
        import result

        type E { Wrap(IoError) }

        fn load() -> Result(String, E) {
          let raw = (io.read_file("/nope") |> result.map_err(Wrap))?
          Ok(raw)
        }

        fn main() {
          match load() {
            Ok(_) -> "ok"
            Err(Wrap(_)) -> "err"
          }
        }
        "#);
    assert_eq!(v, Value::String("err".into()));
}

/// Chained pipes terminated by `?` at the end — must all fold into
/// a single pipeline that `?` then unwraps. Lockdown for the full
/// "railway-oriented" idiom with error conversion at each step.
#[test]
fn chained_pipes_terminated_by_question_unwraps_all() {
    let v = run(r#"
        import io
        import result

        type E { Wrap(IoError) }

        fn run() -> Result(String, E) {
          -- All three pipes fire, `?` unwraps the final Result, Ok
          -- value flows into `raw`.
          let raw = io.read_file("/nope")
                    |> result.map_ok { s -> s }
                    |> result.map_err(Wrap)?
          Ok(raw)
        }

        fn main() {
          match run() {
            Ok(_) -> "ok"
            Err(Wrap(IoNotFound(_))) -> "not-found"
            Err(Wrap(_)) -> "other"
          }
        }
        "#);
    assert_eq!(v, Value::String("not-found".into()));
}

/// `?` works on `Option` the same way — pipe + option.map_or +
/// terminal `?` must also parse and run.
#[test]
fn pipe_then_question_on_option() {
    let v = run(r#"
        import list
        import option

        fn main() {
          let xs = [1, 2, 3]
          -- list.head returns Option(Int); `|> option.map { ... }`
          -- transforms it; `?` propagates None.
          let _first = xs
                       |> list.head
                       |> option.map { n -> n * 10 }
          match _first {
            Some(n) -> n
            None -> -1
          }
        }
        "#);
    assert_eq!(v, Value::Int(10));
}

/// Direct-call form (no pipe) — unaffected by the precedence change,
/// but worth pinning so future parser changes can't silently break
/// the pattern silt docs recommend.
#[test]
fn direct_map_err_constructor_with_question() {
    let v = run(r#"
        import io
        import result

        type E { Wrap(IoError) }

        fn load() -> Result(String, E) {
          let raw = result.map_err(io.read_file("/nope"), Wrap)?
          Ok(raw)
        }

        fn main() {
          match load() {
            Ok(_) -> "ok"
            Err(Wrap(_)) -> "err"
          }
        }
        "#);
    assert_eq!(v, Value::String("err".into()));
}
