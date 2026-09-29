//! End-to-end tests for the `stream` builtin module (v0.10).

use std::sync::Arc;

use silt::types::Severity;
use silt::value::Value;

fn run(input: &str) -> Value {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = silt::compiler::Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = silt::vm::Vm::new();
    vm.run(script).expect("runtime error")
}

fn type_errors(input: &str) -> Vec<String> {
    let tokens = silt::lexer::Lexer::new(input)
        .tokenize()
        .expect("lex error");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse error");
    silt::typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| e.message)
        .collect()
}

// ── Sources ────────────────────────────────────────────────────────────

#[test]
fn test_from_list_collects_back() {
    let v = run(r#"
import stream
fn main() { stream.collect(stream.from_list([1, 2, 3])) }
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

#[test]
fn test_from_range_to_count() {
    let v = run(r#"
import stream
fn main() { stream.count(stream.from_range(1, 100)) }
"#);
    assert_eq!(v, Value::Int(100));
}

#[test]
fn test_repeat_with_take() {
    let v = run(r#"
import stream
fn main() { stream.collect(stream.take(stream.repeat("x"), 3)) }
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::String("x".into()),
            Value::String("x".into()),
            Value::String("x".into()),
        ]))
    );
}

#[test]
fn test_unfold_generates_until_none() {
    let v = run(r#"
import stream
fn main() {
  -- Generate 1, 2, 3, 4, 5 then None.
  stream.collect(stream.unfold(1, fn(n) {
    match n > 5 {
      true -> None
      false -> Some((n, n + 1))
    }
  }))
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}

// ── Transforms ─────────────────────────────────────────────────────────

#[test]
fn test_map() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 4)
    |> stream.map(fn(n) { n * 10 })
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
            Value::Int(40),
        ]))
    );
}

#[test]
fn test_filter() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 10)
    |> stream.filter(fn(n) { n % 2 == 0 })
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(2),
            Value::Int(4),
            Value::Int(6),
            Value::Int(8),
            Value::Int(10),
        ]))
    );
}

#[test]
fn test_take_and_drop() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 100)
    |> stream.drop(50)
    |> stream.take(3)
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(51),
            Value::Int(52),
            Value::Int(53),
        ]))
    );
}

#[test]
fn test_take_while() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 100)
    |> stream.take_while(fn(n) { n < 5 })
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
        ]))
    );
}

#[test]
fn test_chunks() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 7)
    |> stream.chunks(3)
    |> stream.collect
}
"#);
    let expected = Value::List(Arc::new(vec![
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)])),
        Value::List(Arc::new(vec![Value::Int(4), Value::Int(5), Value::Int(6)])),
        Value::List(Arc::new(vec![Value::Int(7)])),
    ]));
    assert_eq!(v, expected);
}

#[test]
fn test_scan() {
    let v = run(r#"
import stream
fn main() {
  -- Running sum of 1..=5 = 1, 3, 6, 10, 15
  stream.from_range(1, 5)
    |> stream.scan(0, fn(acc, x) { acc + x })
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(3),
            Value::Int(6),
            Value::Int(10),
            Value::Int(15),
        ]))
    );
}

#[test]
fn test_dedup() {
    let v = run(r#"
import stream
fn main() {
  stream.from_list([1, 1, 2, 2, 2, 3, 1, 1])
    |> stream.dedup
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(1),
        ]))
    );
}

// ── Combinators ────────────────────────────────────────────────────────

#[test]
fn test_concat() {
    let v = run(r#"
import stream
fn main() {
  stream.collect(stream.concat([
    stream.from_list([1, 2]),
    stream.from_list([3, 4]),
    stream.from_list([5]),
  ]))
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}

#[test]
fn test_zip_pairs() {
    let v = run(r#"
import stream
fn main() {
  let a = stream.from_list([1, 2, 3])
  let b = stream.from_list(["x", "y", "z"])
  stream.collect(stream.zip(a, b))
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Tuple(vec![Value::Int(1), Value::String("x".into())]),
            Value::Tuple(vec![Value::Int(2), Value::String("y".into())]),
            Value::Tuple(vec![Value::Int(3), Value::String("z".into())]),
        ]))
    );
}

// ── Sinks ──────────────────────────────────────────────────────────────

#[test]
fn test_fold_sums() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 10)
    |> stream.fold(0, fn(acc, x) { acc + x })
}
"#);
    assert_eq!(v, Value::Int(55));
}

#[test]
fn test_first_and_last() {
    let v = run(r#"
import stream
fn main() {
  let f = stream.first(stream.from_range(1, 10))
  let l = stream.last(stream.from_range(1, 10))
  (f, l)
}
"#);
    assert_eq!(
        v,
        Value::Tuple(vec![
            Value::Variant("Some".into(), vec![Value::Int(1)]),
            Value::Variant("Some".into(), vec![Value::Int(10)]),
        ])
    );
}

#[test]
fn test_first_on_empty() {
    let v = run(r#"
import stream
fn main() { stream.first(stream.from_list([])) }
"#);
    assert_eq!(v, Value::Variant("None".into(), vec![]));
}

// ── Composition ────────────────────────────────────────────────────────

#[test]
fn test_three_step_pipeline() {
    let v = run(r#"
import stream
fn main() {
  stream.from_range(1, 100)
    |> stream.filter(fn(n) { n % 2 == 1 })
    |> stream.map(fn(n) { n * n })
    |> stream.take(4)
    |> stream.collect
}
"#);
    assert_eq!(
        v,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(9),
            Value::Int(25),
            Value::Int(49),
        ]))
    );
}

// ── File I/O ───────────────────────────────────────────────────────────

#[test]
fn test_file_lines_counts_lines() {
    use std::io::Write;
    let dir = std::env::temp_dir().join("silt_stream_tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("file_lines_{}.txt", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
        writeln!(f, "gamma").unwrap();
    }
    let path_str = path.to_string_lossy().to_string().replace('\\', "/");
    let src = format!(
        r#"
import stream
fn main() {{ stream.count(stream.file_lines("{path_str}")) }}
"#
    );
    let v = run(&src);
    assert_eq!(v, Value::Int(3));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_file_lines_emits_err_for_missing() {
    let v = run(r#"
import stream
fn main() {
  stream.first(stream.file_lines("/definitely/not/a/real/path/__silt_test"))
}
"#);
    // Should be Some(Err(_)) — first chunk is the open error.
    let Value::Variant(name, fields) = v else {
        panic!("expected Some(...)")
    };
    assert_eq!(name, "Some");
    assert_eq!(fields.len(), 1);
    let Value::Variant(inner, _) = &fields[0] else {
        panic!("expected inner Variant")
    };
    assert_eq!(inner, "Err");
}

// ── Type-level integration ────────────────────────────────────────────

#[test]
fn test_typechecker_accepts_pipeline() {
    let errs = type_errors(
        r#"
import stream
fn main() {
  let n = stream.from_range(1, 10)
    |> stream.map(fn(x) { x * 2 })
    |> stream.filter(fn(x) { x > 5 })
    |> stream.fold(0, fn(acc, x) { acc + x })
  let _ = n + 1
}
"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}
