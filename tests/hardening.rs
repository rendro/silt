//! Hardening tests: formatter idempotency & roundtrip, builtin edge cases,
//! concurrent panic recovery, and IO error paths.

// `Value` contains variants with interior mutability (Channel), but the
// tests here use only Value::String / Value::Int as BTreeMap keys.
#![allow(clippy::mutable_key_type)]

use silt::compiler::Compiler;
use silt::formatter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::collections::BTreeMap;
use std::sync::Arc;

// ── Helpers ─────────────────────────────────────────────────────────

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

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

// ── Formatter: idempotency over example files ───────────────────────

#[test]
fn test_formatter_idempotent_on_all_examples() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&examples_dir).expect("read examples dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("silt") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path).expect("read file");

        let first = match formatter::format(&source) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: format() failed: {e}"));
                continue;
            }
        };

        let second = match formatter::format(&first) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: second format() failed: {e}"));
                continue;
            }
        };

        if first != second {
            // Find first differing line for a useful error message
            let first_lines: Vec<&str> = first.lines().collect();
            let second_lines: Vec<&str> = second.lines().collect();
            let diff_line = first_lines
                .iter()
                .zip(second_lines.iter())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {}: {a:?} vs {b:?}", i + 1))
                .unwrap_or_else(|| {
                    format!("length {} vs {}", first_lines.len(), second_lines.len())
                });
            failures.push(format!("{name}: not idempotent ({diff_line})"));
        }
    }

    assert!(
        failures.is_empty(),
        "Formatter idempotency failures:\n  {}",
        failures.join("\n  ")
    );
}

// ── Formatter: roundtrip (formatted code still parses) ──────────────

#[test]
fn test_formatter_roundtrip_parses_on_all_examples() {
    let examples_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&examples_dir).expect("read examples dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("silt") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path).expect("read file");

        let formatted = match formatter::format(&source) {
            Ok(f) => f,
            Err(e) => {
                failures.push(format!("{name}: format() failed: {e}"));
                continue;
            }
        };

        // Verify the formatted code still lexes
        let tokens = match Lexer::new(&formatted).tokenize() {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!(
                    "{name}: formatted code fails to lex: {}",
                    e.message
                ));
                continue;
            }
        };

        // Verify it still parses
        if let Err(e) = Parser::new(tokens).parse_program() {
            failures.push(format!(
                "{name}: formatted code fails to parse: {}",
                e.message
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Formatter roundtrip failures:\n  {}",
        failures.join("\n  ")
    );
}

// ── Builtin edge cases: strings ─────────────────────────────────────

#[test]
fn test_string_split_empty_input() {
    assert_eq!(
        run(r#"
import string
fn main() { string.split("", "x") }
        "#),
        Value::List(Arc::new(vec![Value::String("".into())]))
    );
}

#[test]
fn test_string_chars_empty() {
    assert_eq!(
        run(r#"
import string
fn main() { string.chars("") }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_string_repeat_zero() {
    assert_eq!(
        run(r#"
import string
fn main() { string.repeat("x", 0) }
        "#),
        Value::String("".into())
    );
}

#[test]
fn test_string_trim_empty() {
    assert_eq!(
        run(r#"
import string
fn main() { string.trim("") }
        "#),
        Value::String("".into())
    );
}

#[test]
fn test_string_contains_empty_needle() {
    assert_eq!(
        run(r#"
import string
fn main() { string.contains("hello", "") }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_string_replace_empty_pattern() {
    assert_eq!(
        run(r#"
import string
fn main() { string.replace("abc", "", "-") }
        "#),
        Value::String("-a-b-c-".into())
    );
}

#[test]
fn test_string_length_unicode() {
    assert_eq!(
        run(r#"
import string
fn main() { string.length("héllo") }
        "#),
        Value::Int(5)
    );
}

#[test]
fn test_string_is_empty_true() {
    assert_eq!(
        run(r#"
import string
fn main() { string.is_empty("") }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_string_is_empty_false() {
    assert_eq!(
        run(r#"
import string
fn main() { string.is_empty(" ") }
        "#),
        Value::Bool(false)
    );
}

#[test]
fn test_string_slice_out_of_bounds_clamped() {
    assert_eq!(
        run(r#"
import string
fn main() { string.slice("abc", 0, 100) }
        "#),
        Value::String("abc".into())
    );
}

#[test]
fn test_string_slice_start_past_end() {
    assert_eq!(
        run(r#"
import string
fn main() { string.slice("abc", 5, 10) }
        "#),
        Value::String("".into())
    );
}

// ── Builtin edge cases: lists ───────────────────────────────────────

#[test]
fn test_list_map_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.map([], { x -> x + 1 }) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_filter_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.filter([], { x -> x > 0 }) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_fold_empty_returns_init() {
    assert_eq!(
        run(r#"
import list
fn main() { list.fold([], 42) { acc, x -> acc + x } }
        "#),
        Value::Int(42)
    );
}

#[test]
fn test_list_find_empty_returns_none() {
    assert_eq!(
        run(r#"
import list
fn main() { list.find([], { x -> true }) }
        "#),
        Value::Variant("None".into(), vec![])
    );
}

#[test]
fn test_list_head_empty_returns_none() {
    assert_eq!(
        run(r#"
import list
fn main() { list.head([]) }
        "#),
        Value::Variant("None".into(), vec![])
    );
}

#[test]
fn test_list_last_empty_returns_none() {
    assert_eq!(
        run(r#"
import list
fn main() { list.last([]) }
        "#),
        Value::Variant("None".into(), vec![])
    );
}

#[test]
fn test_list_reverse_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.reverse([]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_sort_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.sort([]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_sort_single_element() {
    assert_eq!(
        run(r#"
import list
fn main() { list.sort([42]) }
        "#),
        Value::List(Arc::new(vec![Value::Int(42)]))
    );
}

#[test]
fn test_list_sort_already_sorted() {
    assert_eq!(
        run(r#"
import list
fn main() { list.sort([1, 2, 3, 4, 5]) }
        "#),
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
fn test_list_unique_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.unique([]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_unique_all_same() {
    assert_eq!(
        run(r#"
import list
fn main() { list.unique([1, 1, 1]) }
        "#),
        Value::List(Arc::new(vec![Value::Int(1)]))
    );
}

#[test]
fn test_list_flatten_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.flatten([]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_flatten_nested_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.flatten([[], [], []]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_zip_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.zip([], [1, 2, 3]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_zip_uneven_lengths() {
    assert_eq!(
        run(r#"
import list
fn main() { list.zip([1, 2], [10, 20, 30]) }
        "#),
        Value::List(Arc::new(vec![
            Value::Tuple(vec![Value::Int(1), Value::Int(10)]),
            Value::Tuple(vec![Value::Int(2), Value::Int(20)]),
        ]))
    );
}

#[test]
fn test_list_any_empty_is_false() {
    assert_eq!(
        run(r#"
import list
fn main() { list.any([], { x -> true }) }
        "#),
        Value::Bool(false)
    );
}

#[test]
fn test_list_all_empty_is_true() {
    assert_eq!(
        run(r#"
import list
fn main() { list.all([], { x -> false }) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_list_enumerate_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.enumerate([]) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_take_zero() {
    assert_eq!(
        run(r#"
import list
fn main() { list.take([1, 2, 3], 0) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_drop_all() {
    assert_eq!(
        run(r#"
import list
fn main() { list.drop([1, 2, 3], 3) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_concat_both_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.concat([], []) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

// ── Builtin edge cases: numeric ─────────────────────────────────────

#[test]
fn test_int_parse_invalid_returns_err() {
    assert_eq!(
        run(r#"
import int
import result
fn main() { result.is_err(int.parse("not_a_number")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_int_parse_empty_string_returns_err() {
    assert_eq!(
        run(r#"
import int
import result
fn main() { result.is_err(int.parse("")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_float_parse_empty_string_returns_err() {
    assert_eq!(
        run(r#"
import float
import result
fn main() { result.is_err(float.parse("")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_float_parse_rejects_infinity() {
    assert_eq!(
        run(r#"
import float
import result
fn main() { result.is_err(float.parse("inf")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_float_parse_rejects_nan() {
    assert_eq!(
        run(r#"
import float
import result
fn main() { result.is_err(float.parse("NaN")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_float_round_negative_zero() {
    assert_eq!(
        run(r#"
import float
fn main() { float.round(-0.0) }
        "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_int_abs_zero() {
    assert_eq!(
        run(r#"
import int
fn main() { int.abs(0) }
        "#),
        Value::Int(0)
    );
}

#[test]
fn test_math_sqrt_zero() {
    assert_eq!(
        run(r#"
import math
fn main() { math.sqrt(0.0) }
        "#),
        Value::ExtFloat(0.0)
    );
}

// ── Latent numeric regression guards ────────────────────────────────
// These tests lock in currently-correct behavior for numeric edge
// cases so a future refactor of the arithmetic/builtin layer cannot
// silently regress them. Do NOT change expected values to match new
// behavior — if one of these fails, the underlying change is wrong.

/// `int.abs(i64::MIN)` must error: `-i64::MIN` overflows i64.
/// `-9223372036854775808` can't be written literally (the lexer parses
/// `-` as unary on an unsigned literal that's out of range), so build
/// it via `-9223372036854775807 - 1`.
#[test]
fn test_int_abs_i64_min_overflows() {
    let err = run_err(
        r#"
import int
fn main() {
  let x = -9223372036854775807 - 1
  int.abs(x)
}
    "#,
    );
    assert!(
        err.contains("integer overflow") && err.contains("abs("),
        "expected integer overflow error from int.abs(i64::MIN), got: {err}"
    );
}

#[test]
fn test_int_abs_negative() {
    assert_eq!(
        run(r#"
import int
fn main() { int.abs(-42) }
        "#),
        Value::Int(42)
    );
}

/// `float.round` uses ties-away-from-zero: 0.5 -> 1.0.
#[test]
fn test_float_round_half_positive() {
    assert_eq!(
        run(r#"
import float
fn main() { float.round(0.5) }
        "#),
        Value::Float(1.0)
    );
}

/// `float.round` uses ties-away-from-zero: -0.5 -> -1.0.
#[test]
fn test_float_round_half_negative() {
    assert_eq!(
        run(r#"
import float
fn main() { float.round(-0.5) }
        "#),
        Value::Float(-1.0)
    );
}

#[test]
fn test_math_sqrt_four() {
    assert_eq!(
        run(r#"
import math
fn main() { math.sqrt(4.0) }
        "#),
        Value::ExtFloat(2.0)
    );
}

#[test]
fn test_int_to_float_zero() {
    assert_eq!(
        run(r#"
import int
fn main() { int.to_float(0) }
        "#),
        Value::Float(0.0)
    );
}

/// `int.to_float(i64::MAX)` rounds to the nearest f64, which is 2^63.
/// This pins the conversion so a refactor (e.g. switching to a checked
/// or lossless-only path) can't silently change the result.
#[test]
fn test_int_to_float_i64_max() {
    assert_eq!(
        run(r#"
import int
fn main() { int.to_float(9223372036854775807) }
        "#),
        Value::Float(i64::MAX as f64)
    );
}

// ── Builtin edge cases: map ─────────────────────────────────────────

#[test]
fn test_map_get_missing_key_returns_none() {
    assert_eq!(
        run(r#"
import map
fn main() { map.get(#{}, "missing") }
        "#),
        Value::Variant("None".into(), vec![])
    );
}

#[test]
fn test_map_keys_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.keys(#{}) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_map_values_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.values(#{}) }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_map_length_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.length(#{}) }
        "#),
        Value::Int(0)
    );
}

#[test]
fn test_map_merge_both_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.merge(#{}, #{}) }
        "#),
        Value::Map(Arc::new(std::collections::BTreeMap::new()))
    );
}

#[test]
fn test_map_delete_missing_key() {
    assert_eq!(
        run(r#"
import map
fn main() { map.length(map.delete(#{"a": 1}, "b")) }
        "#),
        Value::Int(1)
    );
}

#[test]
fn test_map_contains_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.contains(#{}, "anything") }
        "#),
        Value::Bool(false)
    );
}

// ── Builtin edge cases: regex ───────────────────────────────────────

#[test]
fn test_regex_find_no_match() {
    assert_eq!(
        run(r#"
import regex
fn main() { regex.find("xyz", "hello") }
        "#),
        Value::Variant("None".into(), vec![])
    );
}

#[test]
fn test_regex_find_all_no_matches() {
    assert_eq!(
        run(r#"
import regex
fn main() { regex.find_all("xyz", "hello") }
        "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_regex_is_match_empty_pattern() {
    assert_eq!(
        run(r#"
import regex
fn main() { regex.is_match("", "hello") }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_regex_split_no_match() {
    assert_eq!(
        run(r#"
import regex
fn main() { regex.split("xyz", "hello") }
        "#),
        Value::List(Arc::new(vec![Value::String("hello".into())]))
    );
}

// ── Concurrent panic recovery ───────────────────────────────────────

#[test]
fn test_task_error_returns_err_on_join() {
    let err = run_err(
        r#"
import task
fn main() {
  let h = task.spawn(fn() {
    let x = 1 / 0
    x
  })
  task.join(h)
}
    "#,
    );
    assert!(
        err.contains("division") || err.contains("zero"),
        "expected division error, got: {err}"
    );
}

#[test]
fn test_scheduler_healthy_after_task_error() {
    assert_eq!(
        run(r#"
import task
fn main() {
  -- First task fails
  let h1 = task.spawn(fn() {
    1 / 0
  })

  -- Second task should still work
  let h2 = task.spawn(fn() {
    42
  })
  task.join(h2)
}
        "#),
        Value::Int(42)
    );
}

#[test]
fn test_multiple_tasks_some_fail() {
    assert_eq!(
        run(r#"
import channel
import list
import task
fn main() {
  let ch = channel.new(10)

  -- Spawn a mix of good and bad tasks
  let good1 = task.spawn(fn() { channel.send(ch, 1) })
  let bad = task.spawn(fn() { 1 / 0 })
  let good2 = task.spawn(fn() { channel.send(ch, 2) })

  task.join(good1)
  task.join(good2)

  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  a + b
}
        "#),
        Value::Int(3)
    );
}

#[test]
fn test_channel_close_wakes_receiver() {
    assert_eq!(
        run(r#"
import channel
import task
fn main() {
  let ch = channel.new(0)
  let sender = task.spawn(fn() {
    channel.close(ch)
  })
  let result = channel.receive(ch)
  task.join(sender)
  match result {
    Closed -> "closed"
    _ -> "unexpected"
  }
}
        "#),
        Value::String("closed".into())
    );
}

#[test]
fn test_channel_send_after_close_errors() {
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.send(ch, 42)
}
    "#,
    );
    assert!(
        err.contains("closed"),
        "expected closed channel error, got: {err}"
    );
}

#[test]
fn test_try_receive_on_empty_channel() {
    assert_eq!(
        run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  match channel.try_receive(ch) {
    Empty -> "empty"
    _ -> "unexpected"
  }
}
        "#),
        Value::String("empty".into())
    );
}

#[test]
fn test_try_receive_on_closed_channel() {
    assert_eq!(
        run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  match channel.try_receive(ch) {
    Closed -> "closed"
    _ -> "unexpected"
  }
}
        "#),
        Value::String("closed".into())
    );
}

// ── IO error paths ──────────────────────────────────────────────────

#[test]
fn test_read_file_nonexistent_returns_err() {
    assert_eq!(
        run(r#"
import io
import result
fn main() { result.is_err(io.read_file("/tmp/silt_test_nonexistent_file_12345.txt")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_read_file_nonexistent_error_message() {
    assert_eq!(
        run(r#"
import io
import result
fn main() {
  match io.read_file("/tmp/silt_test_nonexistent_file_12345.txt") {
    Ok(_) -> "unexpected success"
    Err(msg) -> match {
      msg == "" -> "empty error"
      _ -> "has error message"
    }
  }
}
        "#),
        Value::String("has error message".into())
    );
}

#[test]
fn test_write_file_bad_path_returns_err() {
    assert_eq!(
        run(r#"
import io
import result
fn main() { result.is_err(io.write_file("/nonexistent_dir_12345/file.txt", "data")) }
        "#),
        Value::Bool(true)
    );
}

#[test]
fn test_write_and_read_roundtrip() {
    let tmp = std::env::temp_dir().join("silt_hardening_test_roundtrip.txt");
    let tmp_str = tmp.to_str().unwrap().replace('\\', "/");
    let src = format!(
        r#"
import io
fn main() {{
  let path = "{tmp_str}"
  io.write_file(path, "hello silt")
  match io.read_file(path) {{
    Ok(content) -> content
    Err(e) -> e
  }}
}}"#
    );
    assert_eq!(run(&src), Value::String("hello silt".into()));
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_read_file_empty_path_returns_err() {
    assert_eq!(
        run(r#"
import io
import result
fn main() { result.is_err(io.read_file("")) }
        "#),
        Value::Bool(true)
    );
}

// ── invoke_callable regression tests ───────────────────────────────
// These tests exercise various opcodes through the `invoke_callable`
// path (closures passed to builtins like `list.map`). They serve as
// regression guards for a subsequent VM dispatch refactor.

/// Test A: QuestionMark (Ok) inside list.map — unwraps Ok values.
#[test]
fn test_invoke_callable_question_mark_ok_in_map() {
    assert_eq!(
        run(r#"
import list
fn try_map() {
  Ok(list.map([Ok(1), Ok(2), Ok(3)], fn(x) { x? }))
}
fn main() {
  match try_map() {
    Ok(xs) -> xs
    Err(e) -> e
  }
}
        "#),
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

/// Test B: QuestionMark (Err) inside list.map — closure early return.
/// `?` on an Err inside a closure causes early return from the closure,
/// so `list.map` receives `Err("fail")` as the mapped element result.
/// The enclosing function `try_map` wraps everything in Ok, and the
/// match on Ok extracts the list containing the un-propagated Err.
#[test]
fn test_invoke_callable_question_mark_err_in_map() {
    assert_eq!(
        run(r#"
import list
fn try_map() {
  Ok(list.map([Ok(1), Err("fail"), Ok(3)], fn(x) { x? }))
}
fn main() {
  match try_map() {
    Ok(xs) -> xs
    Err(e) -> e
  }
}
        "#,),
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Variant("Err".into(), vec![Value::String("fail".into())]),
            Value::Int(3),
        ]))
    );
}

/// Test C: QuestionMark (None) inside a closure — Option variant.
/// `?` on None inside a closure causes early return from the closure,
/// so `list.map` receives `None` as the mapped element result.
/// The enclosing function `try_extract` wraps everything in Some,
/// and the match on Some extracts the list containing the un-propagated None.
#[test]
fn test_invoke_callable_question_mark_none_in_map() {
    assert_eq!(
        run(r#"
import list
fn try_extract() {
  Some(list.map([Some(1), None, Some(3)], fn(x) { x? }))
}
fn main() {
  match try_extract() {
    Some(xs) -> xs
    None -> "none"
  }
}
        "#,),
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Variant("None".into(), vec![]),
            Value::Int(3),
        ]))
    );
}

/// Test D: Panic inside a closure passed to list.map.
#[test]
fn test_invoke_callable_panic_in_map() {
    let err = run_err(
        r#"
import list
fn main() {
  list.map([1, 2, 3], fn(x) {
    match x {
      2 -> panic("boom")
      n -> n * 10
    }
  })
}
        "#,
    );
    assert!(
        err.contains("boom"),
        "expected panic message containing 'boom', got: {err}"
    );
}

/// Test E: Nested function call inside a closure passed to a builtin.
#[test]
fn test_invoke_callable_nested_fn_call_in_map() {
    assert_eq!(
        run(r#"
import list
fn double(x) = x * 2
fn main() {
  list.map([1, 2, 3], fn(x) { double(x) + 1 })
}
        "#),
        Value::List(Arc::new(vec![Value::Int(3), Value::Int(5), Value::Int(7)]))
    );
}

/// Test F: Field access and function call on records inside a closure
/// passed to a builtin.
#[test]
fn test_invoke_callable_field_access_in_map() {
    assert_eq!(
        run(r#"
import list
type Point { x: Int, y: Int }
fn sum(p: Point) -> Int = p.x + p.y
fn main() {
  let pts = [Point { x: 1, y: 2 }, Point { x: 3, y: 4 }]
  list.map(pts, fn(p) { sum(p) })
}
        "#),
        Value::List(Arc::new(vec![Value::Int(3), Value::Int(7)]))
    );
}

/// Test G: Return from nested function called inside closure.
#[test]
fn test_invoke_callable_return_in_nested_fn_in_map() {
    assert_eq!(
        run(r#"
import list
fn maybe_double(x) {
  match x > 2 {
    true -> return x * 2
    false -> x
  }
}
fn main() {
  list.map([1, 2, 3, 4], fn(x) { maybe_double(x) })
}
        "#),
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(6),
            Value::Int(8),
        ]))
    );
}

/// Test H: Closure-returning-closure inside list.map (MakeClosure inside invoke_callable).
#[test]
fn test_invoke_callable_closure_returning_closure_in_map() {
    assert_eq!(
        run(r#"
import list
fn main() {
  let adders = list.map([1, 2, 3], fn(n) { fn(x) { x + n } })
  list.map(adders, fn(f) { f(10) })
}
        "#),
        Value::List(Arc::new(vec![
            Value::Int(11),
            Value::Int(12),
            Value::Int(13),
        ]))
    );
}

/// Test I: map.map through invoke_callable.
#[test]
fn test_invoke_callable_map_map() {
    let result = run(r#"
import map
fn main() {
  let m = #{"a": 1, "b": 2}
  map.map(m) { k, v -> (k, v * 10) }
}
        "#);
    let mut expected = BTreeMap::new();
    expected.insert(Value::String("a".into()), Value::Int(10));
    expected.insert(Value::String("b".into()), Value::Int(20));
    assert_eq!(result, Value::Map(Arc::new(expected)));
}

/// Test J: RecordUpdate inside a closure passed to a builtin.
#[test]
fn test_invoke_callable_record_update_in_map() {
    let result = run(r#"
import list
type Config { name: String, value: Int }
fn main() {
  let base = Config { name: "base", value: 0 }
  list.map([1, 2, 3], fn(v) { base.{ value: v } })
}
        "#);
    let make_config = |v: i64| {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_string(), Value::String("base".into()));
        fields.insert("value".to_string(), Value::Int(v));
        Value::Record("Config".to_string(), Arc::new(fields))
    };
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            make_config(1),
            make_config(2),
            make_config(3)
        ]))
    );
}

// ── Cancel-while-blocked ───────────────────────────────────────────

#[test]
fn test_cancel_task_blocked_on_channel_receive() {
    // A task blocked on channel.receive on a rendezvous channel (no sender)
    // should be cleanly cancelled. The test itself is a liveness check:
    // if the cancel-while-blocked path is broken, this test will hang.
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch = channel.new()
  let h = task.spawn(fn() { channel.receive(ch) })
  task.cancel(h)
  task.join(h)
}
    "#,
    );
    assert!(
        err.contains("cancelled"),
        "expected cancellation error, got: {err}"
    );
}

// ── Value ordering: Float Eq/Ord consistency ───────────────────────

#[test]
fn test_float_ord_consistency() {
    // Verify that Value::Float ordering is consistent with equality.
    // Two equal floats must compare as Equal.
    let a = Value::Float(1.5);
    let b = Value::Float(1.5);
    assert_eq!(a, b);
    assert_eq!(a.cmp(&b), std::cmp::Ordering::Equal);

    // Different floats should order correctly.
    let c = Value::Float(2.0);
    assert_eq!(a.cmp(&c), std::cmp::Ordering::Less);
    assert_eq!(c.cmp(&a), std::cmp::Ordering::Greater);
}

// ── Resource-limit: ListConcat combined size ───────────────────────

#[test]
fn test_list_concat_combined_exceeds_materialize_limit() {
    // Two ranges each under the 10M individual limit, but combined > 10M.
    let err = run_err(
        r#"
import list
fn main() {
  list.concat((1..5_000_001), (1..5_000_001))
}
    "#,
    );
    assert!(
        err.contains("concatenated list exceeds maximum size"),
        "expected combined-size error, got: {err}"
    );
}
