use silt::interpreter::Interpreter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use std::rc::Rc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut interp = Interpreter::new();
    interp.run(&program).expect("runtime error")
}

fn run_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut interp = Interpreter::new();
    interp.run(&program).expect("runtime error");
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut interp = Interpreter::new();
    let err = interp.run(&program).expect_err("expected runtime error");
    format!("{err}")
}

// ── Phase 3: Hello World ─────────────────────────────────────────────

#[test]
fn test_hello_world() {
    run_ok(r#"
fn main() {
  println("hello, world")
}
    "#);
}

// ── Phase 3: FizzBuzz ────────────────────────────────────────────────

#[test]
fn test_fizzbuzz_logic() {
    let result = run(r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  let results = [
    fizzbuzz(1),
    fizzbuzz(3),
    fizzbuzz(5),
    fizzbuzz(15),
  ]
  results
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::String("1".into()),
            Value::String("Fizz".into()),
            Value::String("Buzz".into()),
            Value::String("FizzBuzz".into()),
        ]))
    );
}

#[test]
fn test_fizzbuzz_with_pipe() {
    run_ok(r#"
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..101
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
    "#);
}

// ── Phase 3: Error Handling with when and ? ──────────────────────────

#[test]
fn test_question_mark_operator() {
    let result = run(r#"
fn process(x) {
  let val = Ok(x)?
  Ok(val * 2)
}

fn main() {
  match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_question_mark_propagates_error() {
    let result = run(r#"
fn process(x) {
  let val = Err("oops")?
  Ok(val)
}

fn main() {
  match process(1) {
    Ok(_) -> "ok"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(result, Value::String("oops".into()));
}

#[test]
fn test_when_else() {
    let result = run(r#"
fn safe_div(a, b) {
  when Ok(divisor) = if_nonzero(b) else {
    return Err("division by zero")
  }
  Ok(a / divisor)
}

fn if_nonzero(n) {
  match n {
    0 -> Err("zero")
    _ -> Ok(n)
  }
}

fn main() {
  match safe_div(10, 0) {
    Ok(n) -> "got {n}"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(result, Value::String("division by zero".into()));
}

// ── Phase 3: Traits and Pipes ────────────────────────────────────────

#[test]
fn test_enum_and_trait() {
    run_ok(r#"
type Shape {
  Circle(Float)
  Rect(Float, Float)
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  let shapes = [Circle(5.0), Rect(3.0, 4.0), Circle(1.0)]

  shapes
  |> list.map { s -> (s.display(), area(s)) }
  |> list.each { pair -> println("{pair}") }
}
    "#);
}

// ── Phase 3: Record Update and Destructuring ─────────────────────────

#[test]
fn test_record_update() {
    let result = run(r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  let u2 = birthday(u)
  u2.age
}
    "#);
    assert_eq!(result, Value::Int(31));
}

#[test]
fn test_record_filter_map() {
    run_ok(r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let users = [
    User { name: "Alice", age: 30, active: true },
    User { name: "Bob", age: 25, active: false },
  ]

  users
  |> list.filter { u -> u.active }
  |> list.map { u -> birthday(u) }
  |> list.each { u ->
    println("{u.name} is now {u.age}")
  }
}
    "#);
}

// ── Phase 3: Error Handling with string.split and module access ──────

#[test]
fn test_module_access() {
    let result = run(r#"
fn main() {
  let parts = "hello world" |> string.split(" ")
  parts
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
        ]))
    );
}

#[test]
fn test_error_handling_pipeline() {
    run_ok(r#"
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when Ok(port) = port_result else {
    return Err("invalid port number")
  }

  Ok("connecting to {host}:{port}")
}

fn main() {
  match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }

  match parse_config("host=localhost") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }
}
    "#);
}

// ── Phase 3: Match with guards ───────────────────────────────────────

#[test]
fn test_match_guards() {
    let result = run(r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  [classify(-5), classify(0), classify(42)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::String("negative".into()),
            Value::String("zero".into()),
            Value::String("positive".into()),
        ]))
    );
}

// ── Phase 3: Closures and higher-order functions ─────────────────────

#[test]
fn test_fold() {
    let result = run(r#"
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
    "#);
    assert_eq!(result, Value::Int(120));
}

#[test]
fn test_nested_closures() {
    let result = run(r#"
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
    "#);
    assert_eq!(result, Value::Int(15));
}

// ── Phase 3: String interpolation ────────────────────────────────────

#[test]
fn test_string_interpolation_complex() {
    let result = run(r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
    "#);
    assert_eq!(result, Value::String("hello world, the answer is 42".into()));
}

// ── Phase 3: Map literals ────────────────────────────────────────────

#[test]
fn test_map_literal() {
    run_ok(r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  println(m)
}
    "#);
}

// ── Phase 3: Single-expression functions ─────────────────────────────

#[test]
fn test_single_expr_fn() {
    let result = run(r#"
fn square(x) = x * x
fn add(a, b) = a + b

fn main() {
  add(square(3), square(4))
}
    "#);
    assert_eq!(result, Value::Int(25));
}

// ── Phase 3: Shadowing ──────────────────────────────────────────────

#[test]
fn test_shadowing() {
    let result = run(r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
    "#);
    assert_eq!(result, Value::Int(6));
}

// ── Phase 4: Concurrency ────────────────────────────────────────────

#[test]
fn test_chan_send_receive_buffered() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_chan_send_receive_multiple() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  let a = channel.receive(ch)
  let b = channel.receive(ch)
  let c = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_spawn_and_join() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  task.join(producer)
  let msg1 = channel.receive(ch)
  let msg2 = channel.receive(ch)
  "{msg1} {msg2}"
}
    "#);
    assert_eq!(result, Value::String("hello world".into()));
}

#[test]
fn test_spawn_return_value() {
    let result = run(r#"
fn main() {
  let h = task.spawn(fn() {
    42
  })
  task.join(h)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_producer_consumer() {
    run_ok(r#"
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  let consumer = task.spawn(fn() {
    let msg1 = channel.receive(ch)
    let msg2 = channel.receive(ch)
    println("{msg1} {msg2}")
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
}

#[test]
fn test_channel_with_integers() {
    let result = run(r#"
fn main() {
  let ch = channel.new(5)

  let producer = task.spawn(fn() {
    channel.send(ch, 10)
    channel.send(ch, 20)
    channel.send(ch, 30)
  })

  task.join(producer)

  let a = channel.receive(ch)
  let b = channel.receive(ch)
  let c = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_cancel_task() {
    run_ok(r#"
fn main() {
  let h = task.spawn(fn() {
    42
  })
  task.cancel(h)
}
    "#);
}

#[test]
fn test_select_expression() {
    let result = run(r#"
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  channel.send(ch2, "from ch2")

  match channel.select([ch1, ch2]) {
    (^ch1, msg) -> "got from ch1"
    (^ch2, msg) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_select_with_spawn() {
    let result = run(r#"
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([ch1, ch2]) {
    (^ch1, msg) -> msg
    (^ch2, msg) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_unbuffered_channel() {
    let result = run(r#"
fn main() {
  let ch = channel.new()

  let producer = task.spawn(fn() {
    channel.send(ch, 99)
  })

  task.join(producer)
  channel.receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_multiple_spawns() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)

  let h1 = task.spawn(fn() {
    channel.send(ch, 1)
  })

  let h2 = task.spawn(fn() {
    channel.send(ch, 2)
  })

  let h3 = task.spawn(fn() {
    channel.send(ch, 3)
  })

  task.join(h1)
  task.join(h2)
  task.join(h3)

  let a = channel.receive(ch)
  let b = channel.receive(ch)
  let c = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_channel_passing_complex_values() {
    let result = run(r#"
fn main() {
  let ch = channel.new(5)
  channel.send(ch, [1, 2, 3])
  let list = channel.receive(ch)
  list
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))
    );
}

#[test]
fn test_spawn_with_closure_capture() {
    let result = run(r#"
fn main() {
  let x = 10
  let ch = channel.new(10)

  let h = task.spawn(fn() {
    channel.send(ch, x * 2)
  })

  task.join(h)
  channel.receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(20));
}

// ── Channel closing ─────────────────────────────────────────────────

#[test]
fn test_channel_close() {
    // After close, receive on empty channel returns None
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let a = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    None -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_send_on_closed_channel_errors() {
    // Sending on closed channel should error
    let err = run_err(r#"
fn main() {
  let ch = channel.new(10)
  channel.close(ch)
  channel.send(ch, 42)
}
    "#);
    assert!(err.contains("send on closed channel"), "got: {err}");
}

#[test]
fn test_try_send_success() {
    let result = run(r#"
fn main() {
  let ch = channel.new(1)
  channel.try_send(ch, 42)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_try_send_full() {
    let result = run(r#"
fn main() {
  let ch = channel.new(1)
  channel.send(ch, 1)
  channel.try_send(ch, 2)
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_try_receive_some() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(42)]));
}

#[test]
fn test_try_receive_empty() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_channel_module_qualified() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_channel_module_qualified_close() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let a = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    None -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_channel_module_try_send_receive() {
    let result = run(r#"
fn main() {
  let ch = channel.new(1)
  channel.try_send(ch, 99)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(99)]));
}

// ── List pattern matching ───────────────────────────────────────────

#[test]
fn test_list_pattern_empty() {
    let result = run(r#"
fn describe(xs) {
  match xs {
    [] -> "empty"
    _ -> "not empty"
  }
}
fn main() {
  describe([])
}
    "#);
    assert_eq!(result, Value::String("empty".into()));
}

#[test]
fn test_list_pattern_exact() {
    let result = run(r#"
fn main() {
  match [1, 2, 3] {
    [a, b, c] -> a + b + c
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_list_pattern_head_tail() {
    let result = run(r#"
fn first(xs) {
  match xs {
    [head, ..tail] -> Some(head)
    [] -> None
  }
}
fn main() {
  match first([10, 20, 30]) {
    Some(n) -> n
    None -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_list_pattern_recursive_sum() {
    let result = run(r#"
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}
fn main() {
  sum([1, 2, 3, 4, 5])
}
    "#);
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_list_pattern_in_let() {
    let result = run(r#"
fn main() {
  let [a, b, ..rest] = [1, 2, 3, 4, 5]
  a + b
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_list_pattern_nested() {
    let result = run(r#"
fn main() {
  match [Some(1), Some(2), None] {
    [Some(a), Some(b), ..rest] -> a + b
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(3));
}

// ── Or-patterns ─────────────────────────────────────────────────────

#[test]
fn test_or_pattern() {
    let result = run(r#"
fn classify(n) {
  match n {
    0 | 1 -> "binary"
    2 | 3 | 5 | 7 -> "small prime"
    _ -> "other"
  }
}
fn main() {
  [classify(0), classify(3), classify(9)]
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::String("binary".into()),
        Value::String("small prime".into()),
        Value::String("other".into()),
    ])));
}

#[test]
fn test_or_pattern_constructors() {
    let result = run(r#"
type Color { Red, Green, Blue }
fn is_warm(c) {
  match c {
    Red | Green -> true
    Blue -> false
  }
}
fn main() {
  is_warm(Red)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

// ── Range patterns ──────────────────────────────────────────────────

#[test]
fn test_range_pattern() {
    let result = run(r#"
fn classify(n) {
  match n {
    0..9 -> "single digit"
    10..99 -> "double digit"
    _ -> "big"
  }
}
fn main() {
  [classify(5), classify(42), classify(100)]
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::String("single digit".into()),
        Value::String("double digit".into()),
        Value::String("big".into()),
    ])));
}

// ── Map patterns ────────────────────────────────────────────────────

#[test]
fn test_map_pattern() {
    let result = run(r#"
fn get_name(m) {
  match m {
    #{ "name": n } -> n
    _ -> "unknown"
  }
}
fn main() {
  get_name(#{ "name": "Alice", "age": "30" })
}
    "#);
    assert_eq!(result, Value::String("Alice".into()));
}

// ── Tail-Call Optimization ─────────────────────────────────────────

#[test]
fn test_tail_call_optimization() {
    let result = run(r#"
fn count_down(n, acc) {
  match n {
    0 -> acc
    _ -> count_down(n - 1, acc + 1)
  }
}
fn main() {
  count_down(10000, 0)
}
    "#);
    assert_eq!(result, Value::Int(10000));
}

#[test]
fn test_tail_recursive_sum() {
    let result = run(r#"
fn sum_helper(xs, acc) {
  match xs {
    [] -> acc
    [h, ..t] -> sum_helper(t, acc + h)
  }
}
fn main() {
  sum_helper(1..1001, 0)
}
    "#);
    assert_eq!(result, Value::Int(500500));
}

#[test]
fn test_non_tail_call_still_works() {
    let result = run(r#"
fn factorial(n) {
  match n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}
fn main() {
  factorial(10)
}
    "#);
    assert_eq!(result, Value::Int(3628800));
}

// ── List append and concat ──────────────────────────────────────────

#[test]
fn test_list_append() {
    let result = run(r#"
fn main() {
  list.append([1, 2, 3], 4)
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])));
}

#[test]
fn test_list_concat() {
    let result = run(r#"
fn main() {
  list.concat([1, 2], [3, 4])
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])));
}

// ── Stdlib: list.get, string.index_of, string.slice, etc. ──────────

#[test]
fn test_list_get() {
    let result = run(r#"fn main() { list.get([10, 20, 30], 1) }"#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(20)]));
}

#[test]
fn test_list_get_out_of_bounds() {
    let result = run(r#"fn main() { list.get([1, 2], 5) }"#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_string_index_of() {
    let result = run(r#"fn main() { string.index_of("hello world", "world") }"#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(6)]));
}

#[test]
fn test_string_index_of_not_found() {
    let result = run(r#"fn main() { string.index_of("hello", "xyz") }"#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_string_slice() {
    let result = run(r#"fn main() { string.slice("hello world", 0, 5) }"#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_list_take() {
    let result = run(r#"fn main() { list.take([1, 2, 3, 4, 5], 3) }"#);
    assert_eq!(result, Value::List(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)])));
}

#[test]
fn test_list_drop() {
    let result = run(r#"fn main() { list.drop([1, 2, 3, 4, 5], 2) }"#);
    assert_eq!(result, Value::List(Rc::new(vec![Value::Int(3), Value::Int(4), Value::Int(5)])));
}

#[test]
fn test_list_enumerate() {
    let result = run(r#"fn main() { list.enumerate(["a", "b"]) }"#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::Tuple(vec![Value::Int(0), Value::String("a".into())]),
        Value::Tuple(vec![Value::Int(1), Value::String("b".into())]),
    ])));
}

#[test]
fn test_float_min_max() {
    let result = run(r#"fn main() { (float.min(3.14, 2.71), float.max(3.14, 2.71)) }"#);
    assert_eq!(result, Value::Tuple(vec![Value::Float(2.71), Value::Float(3.14)]));
}

// ── sort_by ─────────────────────────────────────────────────────────

#[test]
fn test_sort_by() {
    let result = run(r#"
fn main() {
  let words = ["banana", "apple", "cherry"]
  words |> list.sort_by { w -> string.length(w) }
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::String("apple".into()),
        Value::String("banana".into()),
        Value::String("cherry".into()),
    ])));
}

// ── Match with comparison scrutinee ─────────────────────────────────

#[test]
fn test_match_comparison_scrutinee() {
    let result = run(r#"
fn classify(a, b) {
  match a > b {
    true -> "greater"
    false -> "not greater"
  }
}
fn main() {
  classify(5, 3)
}
    "#);
    assert_eq!(result, Value::String("greater".into()));
}

// ── Guardless match ────────────────────────────────────────────────

#[test]
fn test_guardless_match_basic() {
    let result = run(r#"
fn main() {
  let x = 7
  match {
    x > 10 -> "big"
    x > 5 -> "medium"
    _ -> "small"
  }
}
    "#);
    assert_eq!(result, Value::String("medium".into()));
}

#[test]
fn test_guardless_match_first_wins() {
    let result = run(r#"
fn main() {
  let x = 15
  match {
    x > 5 -> "first"
    x > 10 -> "second"
    _ -> "default"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_guardless_match_default() {
    let result = run(r#"
fn main() {
  match {
    false -> "nope"
    _ -> "default"
  }
}
    "#);
    assert_eq!(result, Value::String("default".into()));
}

#[test]
fn test_guardless_match_as_expression() {
    let result = run(r#"
fn main() {
  let x = 3
  let label = match {
    x > 10 -> "big"
    x > 0 -> "positive"
    _ -> "non-positive"
  }
  label
}
    "#);
    assert_eq!(result, Value::String("positive".into()));
}

#[test]
fn test_normal_match_still_works() {
    let result = run(r#"
fn main() {
  match 42 {
    0 -> "zero"
    _ -> "nonzero"
  }
}
    "#);
    assert_eq!(result, Value::String("nonzero".into()));
}

// ── try() builtin ──────────────────────────────────────────────────

#[test]
fn test_try_catches_panic() {
    let result = run(r#"
fn main() {
  let result = try(fn() { panic("boom") })
  match result {
    Ok(_) -> "ok"
    Err(msg) -> msg
  }
}
    "#);
    assert_eq!(result, Value::String("panic: panic: boom".into()));
}

#[test]
fn test_try_catches_assertion_failure() {
    let result = run(r#"
fn main() {
  let result = try(fn() { test.assert_eq(1, 2) })
  match result {
    Ok(_) -> "passed"
    Err(msg) -> "caught"
  }
}
    "#);
    assert_eq!(result, Value::String("caught".into()));
}

#[test]
fn test_try_returns_ok_on_success() {
    let result = run(r#"
fn main() {
  let result = try(fn() { 42 })
  match result {
    Ok(n) -> n
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_try_enables_negative_testing() {
    let result = run(r#"
fn main() {
  let passed = 0
  let failed = 0

  let r1 = try(fn() { test.assert_eq(1, 1) })
  let passed = match r1 {
    Ok(_) -> passed + 1
    _ -> passed
  }

  let r2 = try(fn() { test.assert_eq(1, 2) })
  let failed = match r2 {
    Err(_) -> failed + 1
    _ -> failed
  }

  let r3 = try(fn() { panic("intentional") })
  let failed = match r3 {
    Err(_) -> failed + 1
    _ -> failed
  }

  (passed, failed)
}
    "#);
    assert_eq!(result, Value::Tuple(vec![Value::Int(1), Value::Int(2)]));
}

// ── list.flat_map ──────────────────────────────────────────────────

#[test]
fn test_list_flat_map() {
    let result = run(r#"
fn main() {
  [1, 2, 3] |> list.flat_map { n -> [n, n * 10] }
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::Int(1), Value::Int(10),
        Value::Int(2), Value::Int(20),
        Value::Int(3), Value::Int(30),
    ])));
}

// ── list.any / list.all ────────────────────────────────────────────

#[test]
fn test_list_any() {
    let result = run(r#"
fn main() {
  [1, 2, 3, 4] |> list.any { x -> x > 3 }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_list_all() {
    let result = run(r#"
fn main() {
  [2, 4, 6] |> list.all { x -> x > 0 }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

// ── string.pad_left / string.pad_right ─────────────────────────────

#[test]
fn test_string_pad_left() {
    let result = run(r#"
fn main() {
  string.pad_left("42", 5, "0")
}
    "#);
    assert_eq!(result, Value::String("00042".into()));
}

#[test]
fn test_string_pad_right() {
    let result = run(r#"
fn main() {
  string.pad_right("hi", 5, ".")
}
    "#);
    assert_eq!(result, Value::String("hi...".into()));
}

// ── Negative literal pattern ───────────────────────────────────────

#[test]
fn test_negative_literal_pattern() {
    let result = run(r#"
fn classify(n) {
  match n {
    -1 -> "minus one"
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}
fn main() {
  classify(-1)
}
    "#);
    assert_eq!(result, Value::String("minus one".into()));
}

#[test]
fn test_negative_float_pattern() {
    let result = run(r#"
fn main() {
  let x = -3.14
  match x {
    -3.14 -> "neg pi"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("neg pi".into()));
}

// ── Pin operator (^) ────────────────────────────────────────────────

#[test]
fn test_pin_basic() {
    let result = run(r#"
fn main() {
  let x = 42
  match 42 {
    ^x -> "matched"
    _ -> "no match"
  }
}
    "#);
    assert_eq!(result, Value::String("matched".into()));
}

#[test]
fn test_pin_mismatch() {
    let result = run(r#"
fn main() {
  let x = 42
  match 99 {
    ^x -> "matched"
    _ -> "no match"
  }
}
    "#);
    assert_eq!(result, Value::String("no match".into()));
}

#[test]
fn test_pin_in_tuple() {
    let result = run(r#"
fn main() {
  let expected = "hello"
  match ("hello", 42) {
    (^expected, n) -> n
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_pin_nested() {
    let result = run(r#"
fn main() {
  let x = 1
  let y = 2
  match (1, (2, 3)) {
    (^x, (^y, z)) -> z
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_pin_with_guard() {
    let result = run(r#"
fn main() {
  let x = 10
  match 10 {
    n when n == x -> "guard match"
    ^x -> "pin match"
    _ -> "no match"
  }
}
    "#);
    assert_eq!(result, Value::String("guard match".into()));
}

#[test]
fn test_pin_channel_equality() {
    let result = run(r#"
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  match ch1 {
    ^ch2 -> "wrong"
    ^ch1 -> "same"
    _ -> "unknown"
  }
}
    "#);
    assert_eq!(result, Value::String("same".into()));
}

#[test]
fn test_pin_sibling_binding() {
    // In (x, ^x), ^x should reference the OUTER x, not the one just bound
    let result = run(r#"
fn main() {
  let x = 1
  match (2, 1) {
    (x, ^x) -> "matched outer x"
    _ -> "no match"
  }
}
    "#);
    assert_eq!(result, Value::String("matched outer x".into()));
}

#[test]
fn test_pin_string() {
    let result = run(r#"
fn main() {
  let cmd = "quit"
  match "quit" {
    ^cmd -> "exit"
    _ -> "continue"
  }
}
    "#);
    assert_eq!(result, Value::String("exit".into()));
}

// ── channel.select ──────────────────────────────────────────────────

#[test]
fn test_channel_select_basic() {
    let result = run(r#"
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)
  channel.send(ch2, "from ch2")

  match channel.select([ch1, ch2]) {
    (^ch2, msg) -> msg
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_channel_select_with_spawn() {
    let result = run(r#"
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([ch1, ch2]) {
    (^ch1, msg) -> msg
    (^ch2, msg) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_channel_select_returns_tuple() {
    let result = run(r#"
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)

  let result = channel.select([ch])
  match result {
    (_, val) -> val
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ── Loop expression ─────────────────────────────────────────────────

#[test]
fn test_loop_sum() {
    let result = run(r#"
fn main() {
  loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(45));
}

#[test]
fn test_loop_collect_squares() {
    let result = run(r#"
fn main() {
  loop i = 0, acc = [] {
    match i >= 5 {
      true -> acc
      _ -> loop(i + 1, list.append(acc, i * i))
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(4),
            Value::Int(9),
            Value::Int(16),
        ]))
    );
}

#[test]
fn test_loop_fibonacci() {
    let result = run(r#"
fn main() {
  loop n = 10, a = 0, b = 1 {
    match n == 0 {
      true -> a
      _ -> loop(n - 1, b, a + b)
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(55));
}

#[test]
fn test_loop_single_binding() {
    let result = run(r#"
fn main() {
  loop i = 0 {
    match i >= 3 {
      true -> i
      _ -> loop(i + 1)
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_loop_as_expression() {
    let result = run(r#"
fn main() {
  let total = loop i = 1, acc = 0 {
    match i > 5 {
      true -> acc
      _ -> loop(i + 1, acc + i)
    }
  }
  total * 2
}
    "#);
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_loop_with_pattern_matching() {
    let result = run(r#"
fn main() {
  let items = [Some(1), None, Some(3), None, Some(5)]
  loop xs = items, acc = 0 {
    match xs {
      [] -> acc
      [head, ..tail] -> {
        let add = match head {
          Some(n) -> n
          None -> 0
        }
        loop(tail, acc + add)
      }
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_loop_arity_mismatch() {
    let result = run_err(r#"
fn main() {
  loop x = 0, y = 0 {
    loop(1)
  }
}
    "#);
    assert!(result.contains("expects 2 argument(s)"));
}

#[test]
fn test_loop_nested() {
    let result = run(r#"
fn main() {
  loop i = 0, total = 0 {
    match i >= 3 {
      true -> total
      _ -> {
        let inner = loop j = 0, sum = 0 {
          match j >= 3 {
            true -> sum
            _ -> loop(j + 1, sum + 1)
          }
        }
        loop(i + 1, total + inner)
      }
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_loop_with_guardless_match() {
    let result = run(r#"
fn main() {
  loop n = 1, acc = [] {
    match {
      n > 5 -> acc
      n % 2 == 0 -> loop(n + 1, list.append(acc, n))
      _ -> loop(n + 1, acc)
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![Value::Int(2), Value::Int(4)]))
    );
}

// ── list.fold_until ────────────────────────────────────────────────

#[test]
fn test_fold_until_stop() {
    let result = run(r#"
fn main() {
  [1, 2, 3, 4, 5]
  |> list.fold_until(0) { acc, x ->
    match acc + x > 6 {
      true -> Stop(acc)
      _ -> Continue(acc + x)
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_fold_until_all_continue() {
    let result = run(r#"
fn main() {
  [1, 2, 3]
  |> list.fold_until(0) { acc, x -> Continue(acc + x) }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_fold_until_immediate_stop() {
    let result = run(r#"
fn main() {
  [1, 2, 3]
  |> list.fold_until(99) { acc, x -> Stop(acc) }
}
    "#);
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_fold_until_find_first_even() {
    let result = run(r#"
fn main() {
  [1, 3, 5, 4, 6]
  |> list.fold_until(None) { acc, x ->
    match x % 2 == 0 {
      true -> Stop(Some(x))
      _ -> Continue(None)
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::Int(4)])
    );
}

// ── list.unfold ─────────────────────────────────────────────────────

#[test]
fn test_unfold_range() {
    let result = run(r#"
fn main() {
  list.unfold(1) { n ->
    match n > 5 {
      true -> None
      _ -> Some((n, n + 1))
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}

#[test]
fn test_unfold_fibonacci() {
    let result = run(r#"
fn main() {
  list.unfold((0, 1)) { state ->
    let (a, b) = state
    match a > 20 {
      true -> None
      _ -> Some((a, (b, a + b)))
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(5),
            Value::Int(8),
            Value::Int(13),
        ]))
    );
}

#[test]
fn test_unfold_empty() {
    let result = run(r#"
fn main() {
  list.unfold(0) { n -> None }
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![])));
}

#[test]
fn test_unfold_powers_of_two() {
    let result = run(r#"
fn main() {
  list.unfold(1) { n ->
    match n > 32 {
      true -> None
      _ -> Some((n, n * 2))
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(4),
            Value::Int(8),
            Value::Int(16),
            Value::Int(32),
        ]))
    );
}

// ── int.to_string ───────────────────────────────────────────────────

#[test]
fn test_int_to_string() {
    let result = run(r#"
fn main() {
  int.to_string(42)
}
    "#);
    assert_eq!(result, Value::String("42".into()));
}

#[test]
fn test_int_to_string_negative() {
    let result = run(r#"
fn main() {
  int.to_string(-7)
}
    "#);
    assert_eq!(result, Value::String("-7".into()));
}

// ── float.to_string ─────────────────────────────────────────────────

#[test]
fn test_float_to_string_no_decimals_arg() {
    let result = run(r#"
fn main() {
  float.to_string(3.14)
}
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

#[test]
fn test_float_to_string_with_decimals() {
    let result = run(r#"
fn main() {
  float.to_string(3.14159, 2)
}
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

#[test]
fn test_float_to_string_zero_decimals() {
    let result = run(r#"
fn main() {
  float.to_string(3.7, 0)
}
    "#);
    assert_eq!(result, Value::String("4".into()));
}

#[test]
fn test_float_to_string_padding() {
    let result = run(r#"
fn main() {
  float.to_string(3.1, 4)
}
    "#);
    assert_eq!(result, Value::String("3.1000".into()));
}

// ── float.to_int ────────────────────────────────────────────────────

#[test]
fn test_float_to_int() {
    let result = run(r#"
fn main() {
  float.to_int(3.7)
}
    "#);
    assert_eq!(result, Value::Int(3));
}

// ── Generic map keys ────────────────────────────────────────────────

#[test]
fn test_map_int_keys() {
    let result = run(r#"
fn main() {
  let m = #{ 1: "one", 2: "two", 3: "three" }
  map.get(m, 2)
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("two".into())])
    );
}

#[test]
fn test_map_bool_keys() {
    let result = run(r#"
fn main() {
  let m = #{ true: "yes", false: "no" }
  map.get(m, false)
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("no".into())])
    );
}

#[test]
fn test_map_mixed_key_operations() {
    let result = run(r#"
fn main() {
  let m = #{ 1: "a", 2: "b" }
  let m2 = map.set(m, 3, "c")
  map.length(m2)
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_map_int_key_delete() {
    let result = run(r#"
fn main() {
  let m = #{ 1: "a", 2: "b", 3: "c" }
  let m2 = map.delete(m, 2)
  map.length(m2)
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_map_keys_returns_non_string() {
    let result = run(r#"
fn main() {
  let m = #{ 1: "one", 2: "two" }
  map.keys(m)
}
    "#);
    assert_eq!(
        result,
        Value::List(Rc::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn test_map_tuple_keys() {
    let result = run(r#"
fn main() {
  let m = #{ (0, 0): "origin", (1, 0): "right", (0, 1): "up" }
  map.get(m, (1, 0))
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("right".into())])
    );
}

#[test]
fn test_map_string_keys_still_work() {
    let result = run(r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  map.get(m, "name")
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("Alice".into())])
    );
}

#[test]
fn test_map_merge_mixed_keys() {
    let result = run(r#"
fn main() {
  let m1 = #{ 1: "a", 2: "b" }
  let m2 = #{ 2: "B", 3: "c" }
  let merged = map.merge(m1, m2)
  map.length(merged)
}
    "#);
    assert_eq!(result, Value::Int(3));
}

// ── Trait where clause enforcement ──────────────────────────────────

#[test]
fn test_where_clause_with_display() {
    run_ok(r#"
type Shape { Circle(Float) Rect(Float, Float) }

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle({r})"
      Rect(w, h) -> "Rect({w}, {h})"
    }
  }
}

fn show(x) where x: Display {
  x.display()
}

fn main() {
  let s = Circle(3.14)
  println(show(s))
}
    "#);
}

#[test]
fn test_where_clause_with_equal() {
    run_ok(r#"
fn are_same(a, b) where a: Equal {
  a == b
}

fn main() {
  are_same(1, 2)
  are_same("hello", "hello")
}
    "#);
}

#[test]
fn test_where_clause_with_compare() {
    run_ok(r#"
fn is_less(a, b) where a: Compare {
  a < b
}

fn main() {
  is_less(1, 2)
  is_less("a", "b")
}
    "#);
}

// ── Typed AST verification ──────────────────────────────────────────

#[test]
fn test_typed_ast_int_literal() {
    let input = r#"
fn main() {
  42
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "body should be typed");
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_string_expr() {
    let input = r#"
fn main() {
  "hello"
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert_eq!(f.body.ty, Some(silt::types::Type::String));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_list() {
    let input = r#"
fn main() {
  [1, 2, 3]
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "body should be typed");
        assert_eq!(
            f.body.ty,
            Some(silt::types::Type::List(Box::new(silt::types::Type::Int)))
        );
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_binary_expr() {
    let input = r#"
fn main() {
  let x = 10
  x + 32
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "main body should be typed");
        // Block containing `let x = 10; x + 32` should resolve to Int
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_function_return_type() {
    // check_fn_body now unifies body type with return type.
    // The function's own body should have a resolved type.
    let input = r#"
fn double(x) {
  x * 2
}

fn main() {
  double(21)
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    silt::typechecker::check(&mut program);

    // double's body (x * 2) should resolve to Int
    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "double body should be typed");
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_return_type_mismatch_caught() {
    // The typechecker should catch return type mismatches
    let input = r#"
fn add(a: Int, b: Int) -> String {
  a + b
}

fn main() {
  add(1, 2)
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens).parse_program().expect("parse");
    let errors = silt::typechecker::check(&mut program);
    assert!(
        errors.iter().any(|e| e.severity == silt::types::Severity::Error),
        "should catch Int vs String return type mismatch"
    );
}

// ── Mixed int/float arithmetic ──────────────────────────────────────

#[test]
fn test_mixed_int_float_add() {
    let result = run(r#"
fn main() { 1 + 2.5 }
    "#);
    assert_eq!(result, Value::Float(3.5));
}

#[test]
fn test_mixed_float_int_sub() {
    let result = run(r#"
fn main() { 10.0 - 3 }
    "#);
    assert_eq!(result, Value::Float(7.0));
}

#[test]
fn test_mixed_int_float_div() {
    let result = run(r#"
fn main() { 7 / 2.0 }
    "#);
    assert_eq!(result, Value::Float(3.5));
}

#[test]
fn test_mixed_arithmetic_in_pipeline() {
    let result = run(r#"
fn main() {
  let total = 100
  let ratio = int.to_float(total) / 3.0
  float.to_string(ratio, 2)
}
    "#);
    assert_eq!(result, Value::String("33.33".into()));
}

// ── Regex module ────────────────────────────────────────────────────

#[test]
fn test_regex_is_match() {
    let result = run(r#"
fn main() {
  regex.is_match("\\d+", "abc 123 def")
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_regex_is_match_no_match() {
    let result = run(r#"
fn main() {
  regex.is_match("\\d+", "no numbers here")
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_regex_find() {
    let result = run(r#"
fn main() {
  regex.find("\\d+", "abc 123 def 456")
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::String("123".into())]));
}

#[test]
fn test_regex_find_all() {
    let result = run(r#"
fn main() {
  regex.find_all("\\d+", "abc 123 def 456")
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::String("123".into()),
        Value::String("456".into()),
    ])));
}

#[test]
fn test_regex_split() {
    let result = run(r#"
fn main() {
  regex.split("\\s+", "hello   world   foo")
}
    "#);
    assert_eq!(result, Value::List(Rc::new(vec![
        Value::String("hello".into()),
        Value::String("world".into()),
        Value::String("foo".into()),
    ])));
}

#[test]
fn test_regex_replace() {
    let result = run(r#"
fn main() {
  regex.replace("\\d+", "abc 123 def 456", "NUM")
}
    "#);
    assert_eq!(result, Value::String("abc NUM def 456".into()));
}

#[test]
fn test_regex_replace_all() {
    let result = run(r#"
fn main() {
  regex.replace_all("\\d+", "abc 123 def 456", "NUM")
}
    "#);
    assert_eq!(result, Value::String("abc NUM def NUM".into()));
}

// ── JSON module ─────────────────────────────────────────────────────

#[test]
fn test_json_parse_object() {
    let result = run(r#"
fn main() {
  match json.parse("\{\"name\": \"Alice\", \"age\": 30\}") {
    Ok(data) -> map.get(data, "name")
    Err(e) -> None
  }
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::String("Alice".into())]));
}

#[test]
fn test_json_parse_array() {
    let result = run(r#"
fn main() {
  match json.parse("[1, 2, 3]") {
    Ok(data) -> list.length(data)
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_json_parse_error() {
    let result = run(r#"
fn main() {
  match json.parse("not json") {
    Ok(_) -> false
    Err(_) -> true
  }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_json_stringify() {
    let result = run(r#"
fn main() {
  let data = #{ "name": "Bob", "age": 25 }
  json.stringify(data)
}
    "#);
    // BTreeMap sorts keys, so output is deterministic
    let s = match result { Value::String(s) => s, _ => panic!("expected string") };
    assert!(s.contains("\"name\""));
    assert!(s.contains("\"Bob\""));
    assert!(s.contains("\"age\""));
}

#[test]
fn test_json_roundtrip() {
    let result = run(r#"
fn main() {
  let original = #{ "x": 1, "y": 2 }
  let text = json.stringify(original)
  match json.parse(text) {
    Ok(parsed) -> map.get(parsed, "x")
    Err(_) -> None
  }
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(1)]));
}

#[test]
fn test_json_pretty() {
    let result = run(r#"
fn main() {
  let data = #{ "a": 1 }
  json.pretty(data)
}
    "#);
    let s = match result { Value::String(s) => s, _ => panic!("expected string") };
    assert!(s.contains('\n'), "pretty output should have newlines");
}

#[test]
fn test_json_null_handling() {
    let result = run(r#"
fn main() {
  match json.parse("null") {
    Ok(val) -> match val {
      None -> "got none"
      _ -> "other"
    }
    Err(_) -> "error"
  }
}
    "#);
    assert_eq!(result, Value::String("got none".into()));
}

#[test]
fn test_json_nested() {
    let result = run(r#"
fn main() {
  match json.parse("\{\"users\": [\{\"name\": \"A\"\}, \{\"name\": \"B\"\}]\}") {
    Ok(data) -> {
      match map.get(data, "users") {
        Some(users) -> list.length(users)
        None -> 0
      }
    }
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(2));
}
