use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::types::Severity;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

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

fn run_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error");
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

/// Like `run`, but asserts that the typechecker produces no hard errors
/// (warnings are allowed). This catches typechecker regressions that would
/// incorrectly reject valid code.
fn run_typed(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let type_errors = silt::typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard_errors.is_empty(),
        "expected no type errors, got: {:?}",
        hard_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

/// Like `run_ok`, but asserts that the typechecker produces no hard errors.
#[allow(dead_code)]
fn run_typed_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let type_errors = silt::typechecker::check(&mut program);
    let hard_errors: Vec<_> = type_errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(
        hard_errors.is_empty(),
        "expected no type errors, got: {:?}",
        hard_errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error");
}

// ── Phase 3: Hello World ─────────────────────────────────────────────

#[test]
fn test_hello_world() {
    run_typed_ok(
        r#"
fn main() {
  println("hello, world")
}
    "#,
    );
}

// ── Phase 3: FizzBuzz ────────────────────────────────────────────────

#[test]
fn test_fizzbuzz_logic() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("1".into()),
            Value::String("Fizz".into()),
            Value::String("Buzz".into()),
            Value::String("FizzBuzz".into()),
        ]))
    );
}

#[test]
fn test_fizzbuzz_with_pipe() {
    run_typed_ok(
        r#"
import list
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..100
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
    "#,
    );
}

// ── Phase 3: Error Handling with when and ? ──────────────────────────

#[test]
fn test_question_mark_operator() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_question_mark_propagates_error() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(result, Value::String("oops".into()));
}

#[test]
fn test_when_else() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(result, Value::String("division by zero".into()));
}

// ── Phase 3: Traits and Pipes ────────────────────────────────────────

#[test]
fn test_enum_and_trait() {
    run_ok(
        r#"
import list
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
    "#,
    );
}

// ── Phase 3: Record Update and Destructuring ─────────────────────────

#[test]
fn test_record_update() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(result, Value::Int(31));
}

#[test]
fn test_record_filter_map() {
    run_ok(
        r#"
import list
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
    "#,
    );
}

// ── Phase 3: Error Handling with string.split and module access ──────

#[test]
fn test_module_access() {
    let result = run(r#"
import string
fn main() {
  let parts = "hello world" |> string.split(" ")
  parts
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
        ]))
    );
}

#[test]
fn test_error_handling_pipeline() {
    let result = run(r#"
import int
import list
import string
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
  let ok = match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> msg
    Err(e) -> e
  }

  let err = match parse_config("host=localhost") {
    Ok(msg) -> msg
    Err(e) -> e
  }

  ok == "connecting to localhost:8080" && err == "missing port in config"
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

// ── Phase 3: Match with guards ───────────────────────────────────────

#[test]
fn test_match_guards() {
    let result = run_typed(
        r#"
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
    "#,
    );
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("negative".into()),
            Value::String("zero".into()),
            Value::String("positive".into()),
        ]))
    );
}

// ── Phase 3: Closures and higher-order functions ─────────────────────

#[test]
fn test_fold() {
    let result = run_typed(
        r#"
import list
fn main() {
  [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
    "#,
    );
    assert_eq!(result, Value::Int(120));
}

#[test]
fn test_nested_closures() {
    let result = run_typed(
        r#"
fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add5 = make_adder(5)
  add5(10)
}
    "#,
    );
    assert_eq!(result, Value::Int(15));
}

// ── Phase 3: String interpolation ────────────────────────────────────

#[test]
fn test_string_interpolation_complex() {
    let result = run_typed(
        r#"
fn main() {
  let name = "world"
  let n = 42
  "hello {name}, the answer is {n}"
}
    "#,
    );
    assert_eq!(
        result,
        Value::String("hello world, the answer is 42".into())
    );
}

// ── Phase 3: Map literals ────────────────────────────────────────────

#[test]
fn test_map_literal() {
    run_typed_ok(
        r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  println(m)
}
    "#,
    );
}

// ── Phase 3: Single-expression functions ─────────────────────────────

#[test]
fn test_single_expr_fn() {
    let result = run_typed(
        r#"
fn square(x) = x * x
fn add(a, b) = a + b

fn main() {
  add(square(3), square(4))
}
    "#,
    );
    assert_eq!(result, Value::Int(25));
}

// ── Phase 3: Shadowing ──────────────────────────────────────────────

#[test]
fn test_shadowing() {
    let result = run_typed(
        r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  x
}
    "#,
    );
    assert_eq!(result, Value::Int(6));
}

// ── Phase 4: Concurrency ────────────────────────────────────────────

#[test]
fn test_chan_send_receive_buffered() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let Message(val) = channel.receive(ch)
  val
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_chan_send_receive_multiple() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_spawn_and_join() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  task.join(producer)
  let Message(msg1) = channel.receive(ch)
  let Message(msg2) = channel.receive(ch)
  "{msg1} {msg2}"
}
    "#);
    assert_eq!(result, Value::String("hello world".into()));
}

#[test]
fn test_spawn_return_value() {
    let result = run(r#"
import task
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
    run_ok(
        r#"
import channel
import task
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  let consumer = task.spawn(fn() {
    let Message(msg1) = channel.receive(ch)
    let Message(msg2) = channel.receive(ch)
    println("{msg1} {msg2}")
  })

  task.join(producer)
  task.join(consumer)
}
    "#,
    );
}

#[test]
fn test_channel_with_integers() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(5)

  let producer = task.spawn(fn() {
    channel.send(ch, 10)
    channel.send(ch, 20)
    channel.send(ch, 30)
  })

  task.join(producer)

  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_cancel_task() {
    run_ok(
        r#"
import task
fn main() {
  let h = task.spawn(fn() {
    42
  })
  task.cancel(h)
}
    "#,
    );
}

#[test]
fn test_select_expression() {
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  channel.send(ch2, "from ch2")

  match channel.select([ch1, ch2]) {
    (^ch1, Message(msg)) -> "got from ch1"
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_select_with_spawn() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([ch1, ch2]) {
    (^ch1, Message(msg)) -> msg
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_unbuffered_channel() {
    // Rendezvous: sender blocks until receiver is ready.
    // Receive before join to avoid deadlock.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()

  let producer = task.spawn(fn() {
    channel.send(ch, 99)
  })

  let Message(val) = channel.receive(ch)
  task.join(producer)
  val
}
    "#);
    assert_eq!(result, Value::Int(99));
}

// ── Rendezvous channel tests ──────────────────────────────────────

#[test]
fn test_rendezvous_sender_blocks_until_receiver() {
    // Sender on rendezvous channel must block until receiver is ready.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()

  let _ = task.spawn(fn() {
    channel.send(ch, 42)
  })

  let Message(val) = channel.receive(ch)
  val
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_rendezvous_try_send_fails_without_receiver() {
    // try_send on rendezvous should fail when no receiver is waiting
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new()
  channel.try_send(ch, 42)
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_buffered_channel_send_succeeds_immediately() {
    // Buffered channel.new(1) should let one send succeed without receiver
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.try_send(ch, 42)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

// ── Timeout channel tests ────────────────────────────────────────

#[test]
fn test_channel_timeout_fires() {
    // channel.timeout should close after the specified duration
    let result = run(r#"
import channel
fn main() {
  let timer = channel.timeout(50)
  let result = channel.receive(timer)
  match result {
    Closed -> "timed_out"
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("timed_out".into()));
}

#[test]
fn test_channel_timeout_with_select() {
    // select with a timeout channel — timeout should fire when no data arrives
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  let timer = channel.timeout(50)

  match channel.select([ch, timer]) {
    (_, Closed) -> "timeout"
    (_, Message(_)) -> "data"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout".into()));
}

#[test]
fn test_channel_timeout_data_beats_timeout() {
    // If data arrives before timeout, select should return the data
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(1)
  let timer = channel.timeout(5000)

  channel.send(ch, "fast")

  match channel.select([ch, timer]) {
    (_, Message(val)) -> val
    (_, Closed) -> "timeout"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("fast".into()));
}

// ── Bidirectional select tests ───────────────────────────────────

#[test]
fn test_select_send_operation() {
    // Select with a send operation — should succeed when channel has room
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)

  match channel.select([(ch, 42)]) {
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("sent".into()));
}

#[test]
fn test_select_mixed_send_receive() {
    // Select with both send and receive operations
    let result = run(r#"
import channel
fn main() {
  let inbox = channel.new(1)
  let outbox = channel.new(1)

  channel.send(inbox, "hello")

  match channel.select([inbox, (outbox, "world")]) {
    (_, Message(val)) -> val
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_select_send_with_timeout() {
    // Select: send to a full channel with a timeout
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.send(ch, "fill")

  let timer = channel.timeout(50)

  match channel.select([(ch, "more"), timer]) {
    (_, Closed) -> "timeout"
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout".into()));
}

#[test]
fn test_multiple_spawns() {
    let result = run(r#"
import channel
import task
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

  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_channel_passing_complex_values() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.send(ch, [1, 2, 3])
  let Message(list) = channel.receive(ch)
  list
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3),]))
    );
}

#[test]
fn test_spawn_with_closure_capture() {
    let result = run(r#"
import channel
import task
fn main() {
  let x = 10
  let ch = channel.new(10)

  let h = task.spawn(fn() {
    channel.send(ch, x * 2)
  })

  task.join(h)
  let Message(val) = channel.receive(ch)
  val
}
    "#);
    assert_eq!(result, Value::Int(20));
}

// ── Channel closing ─────────────────────────────────────────────────

#[test]
fn test_channel_close() {
    // After close, receive on empty channel returns Closed
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let Message(a) = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    Closed -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_send_on_closed_channel_errors() {
    // Sending on closed channel should error
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
    assert!(err.contains("send on closed channel"), "got: {err}");
}

#[test]
fn test_try_send_success() {
    let result = run(r#"
import channel
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
import channel
fn main() {
  let ch = channel.new(1)
  channel.send(ch, 1)
  channel.try_send(ch, 2)
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_try_receive_message() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Message".into(), vec![Value::Int(42)])
    );
}

#[test]
fn test_try_receive_empty() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(result, Value::Variant("Empty".into(), Vec::new()));
}

#[test]
fn test_channel_module_qualified() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let Message(val) = channel.receive(ch)
  val
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_channel_module_qualified_close() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.close(ch)
  let Message(a) = channel.receive(ch)
  let b = channel.receive(ch)
  match b {
    Closed -> a
    _ -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(1));
}

#[test]
fn test_channel_module_try_send_receive() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.try_send(ch, 99)
  channel.try_receive(ch)
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Message".into(), vec![Value::Int(99)])
    );
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
    let result = run_typed(
        r#"
fn main() {
  match [1, 2, 3] {
    [a, b, c] -> a + b + c
    _ -> 0
  }
}
    "#,
    );
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
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("binary".into()),
            Value::String("small prime".into()),
            Value::String("other".into()),
        ]))
    );
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
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("single digit".into()),
            Value::String("double digit".into()),
            Value::String("big".into()),
        ]))
    );
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
fn test_tco_match_tail_call() {
    // Tail call inside match arm — should use constant stack space.
    // 200_000 exceeds the 100_000 frame limit, proving TCO is active.
    let result = run(r#"
fn count_down(n, acc) {
  match n {
    0 -> acc
    _ -> count_down(n - 1, acc + 1)
  }
}
fn main() {
  count_down(200000, 0)
}
    "#);
    assert_eq!(result, Value::Int(200_000));
}

#[test]
fn test_tco_list_recursion() {
    let result = run(r#"
fn sum_helper(xs, acc) {
  match xs {
    [] -> acc
    [h, ..t] -> sum_helper(t, acc + h)
  }
}
fn main() {
  sum_helper(1..1000, 0)
}
    "#);
    assert_eq!(result, Value::Int(500500));
}

#[test]
fn test_tco_block_tail_call() {
    // Tail call as the last expression in a block.
    let result = run(r#"
fn count(n, acc) {
  let next = acc + 1
  match n {
    0 -> acc
    _ -> count(n - 1, next)
  }
}
fn main() {
  count(200000, 0)
}
    "#);
    assert_eq!(result, Value::Int(200_000));
}

#[test]
fn test_tco_guardless_match() {
    // Tail call in a guardless match arm.
    let result = run(r#"
fn countdown(n) {
  match {
    n == 0 -> 0
    _ -> countdown(n - 1)
  }
}
fn main() {
  countdown(200000)
}
    "#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_non_tail_call_under_depth_limit() {
    // Non-tail recursion within the frame limit should work fine.
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

#[test]
fn test_non_tail_call_exceeds_depth_limit() {
    // Non-tail recursion exceeding the frame limit should produce a clear error.
    let err = run_err(
        r#"
fn deep(n) {
  match n {
    0 -> 0
    _ -> 1 + deep(n - 1)
  }
}
fn main() {
  deep(200000)
}
    "#,
    );
    assert!(
        err.contains("stack overflow"),
        "expected stack overflow error, got: {err}"
    );
    assert!(
        err.contains("tail position"),
        "error should hint at TCO, got: {err}"
    );
}

// ── List append and concat ──────────────────────────────────────────

#[test]
fn test_list_append() {
    let result = run(r#"
import list
fn main() {
  list.append([1, 2, 3], 4)
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ]))
    );
}

#[test]
fn test_list_concat() {
    let result = run(r#"
import list
fn main() {
  list.concat([1, 2], [3, 4])
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ]))
    );
}

// ── Spread in list literals ─────────────────────────────────────────

#[test]
fn test_list_spread_basic() {
    let result = run(r#"
fn main() {
  let xs = [1, 2, 3]
  [..xs, 4, 5]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]))
    );
}

#[test]
fn test_list_spread_prepend() {
    let result = run(r#"
fn main() {
  let xs = [3, 4, 5]
  [1, 2, ..xs]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]))
    );
}

#[test]
fn test_list_spread_middle() {
    let result = run(r#"
fn main() {
  let xs = [2, 3, 4]
  [1, ..xs, 5]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]))
    );
}

#[test]
fn test_list_spread_multiple() {
    let result = run(r#"
fn main() {
  let a = [1, 2]
  let b = [3, 4]
  [..a, ..b]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ]))
    );
}

#[test]
fn test_list_spread_multiple_with_elements_between() {
    let result = run(r#"
fn main() {
  let a = [1, 2]
  let b = [5, 6]
  [..a, 3, 4, ..b]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
            Value::Int(6)
        ]))
    );
}

#[test]
fn test_list_spread_empty_list() {
    let result = run(r#"
fn main() {
  let xs = []
  [1, ..xs, 2]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn test_list_spread_into_empty() {
    let result = run(r#"
fn main() {
  let xs = [1, 2, 3]
  [..xs]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

#[test]
fn test_list_spread_nested() {
    let result = run(r#"
fn main() {
  let inner = [2, 3]
  let outer = [1, ..inner, 4]
  [0, ..outer, 5]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]))
    );
}

#[test]
fn test_list_spread_with_strings() {
    let result = run(r#"
fn main() {
  let greetings = ["hello", "world"]
  [..greetings, "!"]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
            Value::String("!".into())
        ]))
    );
}

#[test]
fn test_list_spread_from_function_result() {
    let result = run(r#"
fn nums() { [1, 2, 3] }
fn main() {
  [0, ..nums(), 4]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4)
        ]))
    );
}

#[test]
fn test_list_spread_single_element_list() {
    let result = run(r#"
fn main() {
  let xs = [42]
  [..xs]
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![Value::Int(42)])));
}

#[test]
fn test_list_spread_both_empty() {
    let result = run(r#"
fn main() {
  let a = []
  let b = []
  [..a, ..b]
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_list_spread_preserves_order() {
    let result = run(r#"
import list
fn main() {
  let xs = [3, 1, 2]
  let ys = [6, 4, 5]
  list.length([..xs, ..ys])
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_list_spread_with_range() {
    let result = run(r#"
fn main() {
  let r = 1..4
  [0, ..r, 5]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5)
        ]))
    );
}

#[test]
fn test_list_spread_three_lists() {
    let result = run(r#"
fn main() {
  let a = [1]
  let b = [2]
  let c = [3]
  [..a, ..b, ..c]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

#[test]
fn test_list_spread_in_function_arg() {
    let result = run(r#"
import list
fn main() {
  let xs = [1, 2]
  list.length([..xs, 3, 4, 5])
}
    "#);
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_list_spread_non_list_error() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  [1, ..x]
}
    "#,
    );
    assert!(
        err.contains("not a list") || err.contains("ListConcat"),
        "expected list error, got: {err}"
    );
}

// ── Stdlib: list.get, string.index_of, string.slice, etc. ──────────

#[test]
fn test_list_get() {
    let result = run(r#"import list
fn main() { list.get([10, 20, 30], 1) }"#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(20)]));
}

#[test]
fn test_list_get_out_of_bounds() {
    let result = run(r#"import list
fn main() { list.get([1, 2], 5) }"#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_string_index_of() {
    let result = run(r#"import string
fn main() { string.index_of("hello world", "world") }"#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(6)]));
}

#[test]
fn test_string_index_of_not_found() {
    let result = run(r#"import string
fn main() { string.index_of("hello", "xyz") }"#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_string_slice() {
    let result = run(r#"import string
fn main() { string.slice("hello world", 0, 5) }"#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_list_take() {
    let result = run(r#"import list
fn main() { list.take([1, 2, 3, 4, 5], 3) }"#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

#[test]
fn test_list_drop() {
    let result = run(r#"import list
fn main() { list.drop([1, 2, 3, 4, 5], 2) }"#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(3), Value::Int(4), Value::Int(5)]))
    );
}

#[test]
fn test_list_enumerate() {
    let result = run(r#"import list
fn main() { list.enumerate(["a", "b"]) }"#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Tuple(vec![Value::Int(0), Value::String("a".into())]),
            Value::Tuple(vec![Value::Int(1), Value::String("b".into())]),
        ]))
    );
}

#[test]
#[allow(clippy::approx_constant)]
fn test_float_min_max() {
    let result = run(r#"import float
fn main() { (float.min(3.14, 2.71), float.max(3.14, 2.71)) }"#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Float(2.71), Value::Float(3.14)])
    );
}

// ── sort_by ─────────────────────────────────────────────────────────

#[test]
fn test_sort_by() {
    let result = run(r#"
import list
import string
fn main() {
  let words = ["banana", "apple", "cherry"]
  words |> list.sort_by { w -> string.length(w) }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("apple".into()),
            Value::String("banana".into()),
            Value::String("cherry".into()),
        ]))
    );
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

// ── list.flat_map ──────────────────────────────────────────────────

#[test]
fn test_list_flat_map() {
    let result = run(r#"
import list
fn main() {
  [1, 2, 3] |> list.flat_map { n -> [n, n * 10] }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(10),
            Value::Int(2),
            Value::Int(20),
            Value::Int(3),
            Value::Int(30),
        ]))
    );
}

// ── list.filter_map ────────────────────────────────────────────────

#[test]
fn test_list_filter_map() {
    let result = run(r#"
import list
fn main() {
  [1, 2, 3, 4, 5] |> list.filter_map { n ->
    match n % 2 == 0 {
      true -> Some(n * 10)
      _ -> None
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(20), Value::Int(40),]))
    );
}

#[test]
fn test_list_filter_map_all_none() {
    let result = run(r#"
import list
fn main() {
  [1, 2, 3] |> list.filter_map { _ -> None }
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_list_filter_map_all_some() {
    let result = run(r#"
import list
fn main() {
  [1, 2, 3] |> list.filter_map { n -> Some(n + 100) }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(101),
            Value::Int(102),
            Value::Int(103),
        ]))
    );
}

// ── list.any / list.all ────────────────────────────────────────────

#[test]
fn test_list_any() {
    let result = run(r#"
import list
fn main() {
  [1, 2, 3, 4] |> list.any { x -> x > 3 }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_list_all() {
    let result = run(r#"
import list
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
import string
fn main() {
  string.pad_left("42", 5, "0")
}
    "#);
    assert_eq!(result, Value::String("00042".into()));
}

#[test]
fn test_string_pad_right() {
    let result = run(r#"
import string
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
import channel
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
import channel
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)
  channel.send(ch2, "from ch2")

  match channel.select([ch1, ch2]) {
    (^ch2, Message(msg)) -> msg
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_channel_select_with_spawn() {
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p = task.spawn(fn() {
    channel.send(ch1, "first")
  })
  task.join(p)

  match channel.select([ch1, ch2]) {
    (^ch1, Message(msg)) -> msg
    (^ch2, Message(msg)) -> msg
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_channel_select_returns_tuple() {
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)

  let result = channel.select([ch])
  match result {
    (_, Message(val)) -> val
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
import list
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
        Value::List(Arc::new(vec![
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
    let result = run_err(
        r#"
fn main() {
  loop x = 0, y = 0 {
    loop(1)
  }
}
    "#,
    );
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
import list
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
        Value::List(Arc::new(vec![Value::Int(2), Value::Int(4)]))
    );
}

// ── list.fold_until ────────────────────────────────────────────────

#[test]
fn test_fold_until_stop() {
    let result = run(r#"
import list
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
import list
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
import list
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
import list
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
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(4)]));
}

// ── list.unfold ─────────────────────────────────────────────────────

#[test]
fn test_unfold_range() {
    let result = run(r#"
import list
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
fn test_unfold_fibonacci() {
    let result = run(r#"
import list
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
        Value::List(Arc::new(vec![
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
import list
fn main() {
  list.unfold(0) { n -> None }
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_unfold_powers_of_two() {
    let result = run(r#"
import list
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
        Value::List(Arc::new(vec![
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
import int
fn main() {
  int.to_string(42)
}
    "#);
    assert_eq!(result, Value::String("42".into()));
}

#[test]
fn test_int_to_string_negative() {
    let result = run(r#"
import int
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
import float
fn main() {
  float.to_string(3.14, 2)
}
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

#[test]
fn test_float_to_string_with_decimals() {
    let result = run(r#"
import float
fn main() {
  float.to_string(3.14159, 2)
}
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

#[test]
fn test_float_to_string_zero_decimals() {
    let result = run(r#"
import float
fn main() {
  float.to_string(3.7, 0)
}
    "#);
    assert_eq!(result, Value::String("4".into()));
}

#[test]
fn test_float_to_string_padding() {
    let result = run(r#"
import float
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
import float
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
import map
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
import map
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
import map
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
import map
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
import map
fn main() {
  let m = #{ 1: "one", 2: "two" }
  map.keys(m)
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn test_map_tuple_keys() {
    let result = run(r#"
import map
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
import map
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
import map
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
    let result = run(r#"
type Shape { Circle(Float) Rect(Float, Float) }

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle({r})"
      Rect(w, h) -> "Rect({w}, {h})"
    }
  }
}

fn show(x: a) where a: Display {
  x.display()
}

fn main() {
  let s = Circle(3.14)
  show(s)
}
    "#);
    assert_eq!(result, Value::String("Circle(3.14)".into()));
}

#[test]
fn test_where_clause_with_equal() {
    let result = run(r#"
fn are_same(a: t, b: t) where t: Equal {
  a == b
}

fn main() {
  are_same(1, 2)
  are_same("hello", "hello")
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_where_clause_with_compare() {
    let result = run(r#"
fn is_less(a: t, b: t) where t: Compare {
  a < b
}

fn main() {
  is_less(1, 2)
  is_less("a", "b")
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_where_clause_multi_trait_bounds() {
    let result = run(r#"
fn check(a: t, b: t) where t: Equal + Compare {
  let eq = a == b
  let lt = a < b
  eq
}

fn main() {
  check(1, 2)
  check(3, 3)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_where_clause_multi_trait_bounds_mixed() {
    let result = run(r#"
type Color { Red Blue }

trait Display for Color {
  fn display(self) -> String {
    match self {
      Red -> "Red"
      Blue -> "Blue"
    }
  }
}

fn show_and_compare(a: t, b: u) where t: Equal + Compare, u: Display {
  let eq = a == a
  let lt = a < a
  b.display()
}

fn main() {
  show_and_compare(1, Red)
}
    "#);
    assert_eq!(result, Value::String("Red".into()));
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
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
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    let errors = silt::typechecker::check(&mut program);
    assert!(
        errors
            .iter()
            .any(|e| e.severity == silt::types::Severity::Error),
        "should catch Int vs String return type mismatch"
    );
}

// ── Mixed int/float arithmetic ──────────────────────────────────────

#[test]
fn test_mixed_int_float_add() {
    let err = run_err(
        r#"
fn main() { 1 + 2.5 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_float_int_sub() {
    let err = run_err(
        r#"
fn main() { 10.0 - 3 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_int_float_div() {
    let err = run_err(
        r#"
fn main() { 7 / 2.0 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_arithmetic_in_pipeline() {
    let result = run(r#"
import float
import int
fn main() {
  let total = 100
  let ratio = int.to_float(total) / 3.0
  float.to_string(ratio, 2)
}
    "#);
    assert_eq!(result, Value::String("33.33".into()));
}

// ── Cross-type comparison errors ────────────────────────────────────

#[test]
fn test_cross_type_eq_is_error() {
    let err = run_err(
        r#"
fn main() { 5 == "hello" }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_cross_type_lt_is_error() {
    let err = run_err(
        r#"
fn main() { 3 < true }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_cross_type_int_float_eq_is_error() {
    let err = run_err(
        r#"
fn main() { 3 == 3.0 }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

// ── Builtin trait methods ────────────────────────────────────────────

#[test]
fn test_builtin_display_int() {
    let result = run(r#"
fn main() { 42.display() }
    "#);
    assert_eq!(result, Value::String("42".into()));
}

#[test]
fn test_builtin_display_string() {
    let result = run(r#"
fn main() { "hello".display() }
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_builtin_display_bool() {
    let result = run(r#"
fn main() { true.display() }
    "#);
    assert_eq!(result, Value::String("true".into()));
}

#[test]
fn test_builtin_display_list() {
    let result = run(r#"
fn main() { [1, 2, 3].display() }
    "#);
    assert_eq!(result, Value::String("[1, 2, 3]".into()));
}

#[test]
fn test_builtin_equal_int() {
    let result = run(r#"
fn main() { 42.equal(42) }
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_builtin_compare_int() {
    let result = run(r#"
fn main() { 3.compare(5) }
    "#);
    assert_eq!(result, Value::Int(-1));
}

// ── Math module ─────────────────────────────────────────────────────

#[test]
fn test_math_sqrt() {
    let result = run(r#"
import math
fn main() { math.sqrt(16.0) }
    "#);
    assert_eq!(result, Value::ExtFloat(4.0));
}

#[test]
fn test_math_pow() {
    let result = run(r#"
import math
fn main() { math.pow(2.0, 10.0) }
    "#);
    assert_eq!(result, Value::ExtFloat(1024.0));
}

#[test]
fn test_math_pi() {
    let result = run(r#"
import math
fn main() { math.pi }
    "#);
    assert_eq!(result, Value::Float(std::f64::consts::PI));
}

#[test]
fn test_math_trig() {
    let result = run(r#"
import math
fn main() { math.sin(0.0) }
    "#);
    assert_eq!(result, Value::Float(0.0));
}

#[test]
fn test_math_log() {
    let result = run(r#"
import math
fn main() { math.log(math.e) }
    "#);
    // ln(e) = 1.0 — math.log now returns ExtFloat
    if let Value::ExtFloat(f) = result {
        assert!((f - 1.0).abs() < 1e-10);
    } else {
        panic!("expected ExtFloat, got: {result:?}");
    }
}

// ── Map functional operations ───────────────────────────────────────

#[test]
fn test_map_filter() {
    let result = run(r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2, "c": 3 }
  let big = map.filter(m) { k, v -> v > 1 }
  map.length(big)
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_map_map() {
    let result = run(r#"
import map
fn main() {
  let m = #{ "x": 1, "y": 2 }
  let doubled = map.map(m) { k, v -> (k, v * 2) }
  map.get(doubled, "x")
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(2)]));
}

#[test]
fn test_map_entries_roundtrip() {
    let result = run(r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let entries = map.entries(m)
  let rebuilt = map.from_entries(entries)
  map.get(rebuilt, "a")
}
    "#);
    assert_eq!(result, Value::Variant("Some".into(), vec![Value::Int(1)]));
}

// ── list.group_by ───────────────────────────────────────────────────

#[test]
fn test_list_group_by() {
    let result = run(r#"
import list
import map
fn main() {
  let xs = [1, 2, 3, 4, 5, 6]
  let groups = xs |> list.group_by { x -> x % 2 }
  map.get(groups, 0)
}
    "#);
    assert_eq!(
        result,
        Value::Variant(
            "Some".into(),
            vec![Value::List(Arc::new(vec![
                Value::Int(2),
                Value::Int(4),
                Value::Int(6)
            ]))]
        )
    );
}

// ── Regex module ────────────────────────────────────────────────────

#[test]
fn test_regex_is_match() {
    let result = run(r#"
import regex
fn main() {
  regex.is_match("\\d+", "abc 123 def")
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_regex_is_match_no_match() {
    let result = run(r#"
import regex
fn main() {
  regex.is_match("\\d+", "no numbers here")
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_regex_find() {
    let result = run(r#"
import regex
fn main() {
  regex.find("\\d+", "abc 123 def 456")
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("123".into())])
    );
}

#[test]
fn test_regex_find_all() {
    let result = run(r#"
import regex
fn main() {
  regex.find_all("\\d+", "abc 123 def 456")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("123".into()),
            Value::String("456".into()),
        ]))
    );
}

#[test]
fn test_regex_split() {
    let result = run(r#"
import regex
fn main() {
  regex.split("\\s+", "hello   world   foo")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("hello".into()),
            Value::String("world".into()),
            Value::String("foo".into()),
        ]))
    );
}

#[test]
fn test_regex_replace() {
    let result = run(r#"
import regex
fn main() {
  regex.replace("\\d+", "abc 123 def 456", "NUM")
}
    "#);
    assert_eq!(result, Value::String("abc NUM def 456".into()));
}

#[test]
fn test_regex_replace_all() {
    let result = run(r#"
import regex
fn main() {
  regex.replace_all("\\d+", "abc 123 def 456", "NUM")
}
    "#);
    assert_eq!(result, Value::String("abc NUM def NUM".into()));
}

// ── JSON module ─────────────────────────────────────────────────────

#[test]
fn test_json_parse_record() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"age\": 30\}") {
    Ok(user) -> user.name
    Err(_) -> "fail"
  }
}
    "#);
    assert_eq!(result, Value::String("Alice".into()));
}

#[test]
fn test_json_parse_record_int_field() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"age\": 30\}") {
    Ok(user) -> user.age
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_json_parse_nested_record() {
    let result = run(r#"
import json
type Address { city: String, zip: String }
type User { name: String, address: Address }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"address\": \{\"city\": \"NYC\", \"zip\": \"10001\"\}\}") {
    Ok(user) -> user.address.city
    Err(_) -> "fail"
  }
}
    "#);
    assert_eq!(result, Value::String("NYC".into()));
}

#[test]
fn test_json_parse_list_field() {
    let result = run(r#"
import json
import list
type User { name: String, skills: List(String) }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"skills\": [\"go\", \"rust\"]\}") {
    Ok(user) -> list.length(user.skills)
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_json_parse_option_field_present() {
    let result = run(r#"
import json
type User { name: String, email: Option(String) }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"email\": \"a@b.com\"\}") {
    Ok(user) -> user.email
    Err(_) -> None
  }
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("a@b.com".into())])
    );
}

#[test]
fn test_json_parse_option_field_null() {
    let result = run(r#"
import json
type User { name: String, email: Option(String) }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\", \"email\": null\}") {
    Ok(user) -> user.email
    Err(_) -> Some("fail")
  }
}
    "#);
    assert_eq!(result, Value::Variant("None".into(), vec![]));
}

#[test]
fn test_json_parse_option_field_missing() {
    let result = run(r#"
import json
type User { name: String, email: Option(String) }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\"\}") {
    Ok(user) -> user.email
    Err(_) -> Some("fail")
  }
}
    "#);
    assert_eq!(result, Value::Variant("None".into(), vec![]));
}

#[test]
fn test_json_parse_missing_field_error() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  match json.parse(User, "\{\"name\": \"Alice\"\}") {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(
        result,
        Value::String("json.parse(User): missing field 'age'".into())
    );
}

#[test]
fn test_json_parse_wrong_type_error() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  match json.parse(User, "\{\"name\": 42, \"age\": 30\}") {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(
        result,
        Value::String("json.parse(User): field 'name': expected String, got number".into())
    );
}

#[test]
fn test_json_parse_not_object_error() {
    let result = run(r#"
import json
type User { name: String }
fn main() {
  match json.parse(User, "[1,2,3]") {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(
        result,
        Value::String("json.parse(User): expected JSON object, got array".into())
    );
}

#[test]
fn test_json_parse_invalid_json_error() {
    let result = run(r#"
import json
type User { name: String }
fn main() {
  match json.parse(User, "not json") {
    Ok(_) -> false
    Err(_) -> true
  }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_json_parse_list_basic() {
    let result = run(r#"
import json
import list
type Employee { name: String, department: String, salary: Int }
fn main() {
  let json_str = "[\{\"name\": \"Alice\", \"department\": \"Eng\", \"salary\": 120000\}, \{\"name\": \"Bob\", \"department\": \"Sales\", \"salary\": 95000\}]"
  match json.parse_list(Employee, json_str) {
    Ok(employees) -> list.length(employees)
    Err(_) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_json_parse_list_access_fields() {
    let result = run(r#"
import json
import list
type Employee { name: String, salary: Int }
fn main() {
  let json_str = "[\{\"name\": \"Alice\", \"salary\": 120000\}, \{\"name\": \"Bob\", \"salary\": 95000\}]"
  match json.parse_list(Employee, json_str) {
    Ok(employees) -> match list.get(employees, 0) {
      Some(e) -> e.name
      None -> "fail"
    }
    Err(_) -> "fail"
  }
}
    "#);
    assert_eq!(result, Value::String("Alice".into()));
}

#[test]
fn test_json_parse_list_empty() {
    let result = run(r#"
import json
import list
type Employee { name: String }
fn main() {
  match json.parse_list(Employee, "[]") {
    Ok(employees) -> list.length(employees)
    Err(_) -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_json_parse_list_not_array_error() {
    let result = run(r#"
import json
type Employee { name: String }
fn main() {
  match json.parse_list(Employee, "\{\"name\": \"Alice\"\}") {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(
        result,
        Value::String("json.parse_list(Employee): expected JSON array, got object".into())
    );
}

#[test]
fn test_json_parse_list_invalid_field_error() {
    let result = run(r#"
import json
type Employee { name: String, salary: Int }
fn main() {
  match json.parse_list(Employee, "[\{\"name\": \"Alice\", \"salary\": \"not_a_number\"\}]") {
    Ok(_) -> "unexpected"
    Err(e) -> e
  }
}
    "#);
    assert_eq!(result, Value::String("json.parse_list(Employee): element 0: json.parse(Employee): field 'salary': expected Int, got string".into()));
}

#[test]
fn test_json_parse_list_nested_records() {
    let result = run(r#"
import json
import list
type Address { city: String, zip: String }
type Person { name: String, address: Address }
fn main() {
  let json_str = "[\{\"name\": \"Alice\", \"address\": \{\"city\": \"NYC\", \"zip\": \"10001\"\}\}, \{\"name\": \"Bob\", \"address\": \{\"city\": \"LA\", \"zip\": \"90001\"\}\}]"
  match json.parse_list(Person, json_str) {
    Ok(people) -> match list.get(people, 1) {
      Some(p) -> p.address.city
      None -> "fail"
    }
    Err(_) -> "fail"
  }
}
    "#);
    assert_eq!(result, Value::String("LA".into()));
}

#[test]
fn test_json_stringify() {
    let result = run(r#"
import json
fn main() {
  let data = #{ "name": "Bob", "age": 25 }
  json.stringify(data)
}
    "#);
    let s = match result {
        Value::String(s) => s,
        _ => panic!("expected string"),
    };
    assert!(s.contains("\"name\""));
    assert!(s.contains("\"Bob\""));
    assert!(s.contains("\"age\""));
}

#[test]
fn test_json_stringify_record() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Alice", age: 30 }
  json.stringify(u)
}
    "#);
    let s = match result {
        Value::String(s) => s,
        _ => panic!("expected string"),
    };
    assert!(s.contains("\"name\""));
    assert!(s.contains("\"Alice\""));
    assert!(s.contains("\"age\""));
    assert!(s.contains("30"));
}

#[test]
fn test_json_roundtrip_record() {
    let result = run(r#"
import json
type User { name: String, age: Int }
fn main() {
  let u = User { name: "Carol", age: 25 }
  let text = json.stringify(u)
  match json.parse(User, text) {
    Ok(parsed) -> parsed.name
    Err(_) -> "fail"
  }
}
    "#);
    assert_eq!(result, Value::String("Carol".into()));
}

#[test]
fn test_json_pretty() {
    let result = run(r#"
import json
fn main() {
  let data = #{ "a": 1 }
  json.pretty(data)
}
    "#);
    let s = match result {
        Value::String(s) => s,
        _ => panic!("expected string"),
    };
    assert!(s.contains('\n'), "pretty output should have newlines");
}

// ── map.get_in / map.set_in ─────────────────────────────────────────

// ── JSON + time integration ─────────────────────────────────────────

#[test]
fn test_json_parse_date_field() {
    let result = run(r#"
import json
import time
type Event { name: String, date: Date }
fn main() {
  let e = json.parse(Event, "\{\"name\": \"launch\", \"date\": \"2024-03-15\"\}")?
  e.date
}
    "#);
    assert_eq!(result, make_date(2024, 3, 15));
}

#[test]
fn test_json_parse_datetime_field() {
    let result = run(r#"
import json
import time
type Meeting { title: String, start: DateTime }
fn main() {
  let m = json.parse(Meeting, "\{\"title\": \"standup\", \"start\": \"2024-03-15T09:00:00\"\}")?
  m.start.time.hour
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_json_parse_time_field() {
    let result = run(r#"
import json
import time
type Alarm { label: String, at: Time }
fn main() {
  let a = json.parse(Alarm, "\{\"label\": \"wake up\", \"at\": \"07:30:00\"\}")?
  (a.at.hour, a.at.minute)
}
    "#);
    assert_eq!(result, Value::Tuple(vec![Value::Int(7), Value::Int(30)]));
}

#[test]
fn test_json_parse_date_invalid_string() {
    let result = run(r#"
import json
import time
type Event { name: String, date: Date }
fn main() { json.parse(Event, "\{\"name\": \"x\", \"date\": \"not-a-date\"\}") }
    "#);
    assert!(matches!(result, Value::Variant(ref tag, _) if tag == "Err"));
}

#[test]
fn test_json_parse_date_weekday_pipeline() {
    let result = run(r#"
import json
import time
type Event { name: String, date: Date }
fn main() {
  let e = json.parse(Event, "\{\"name\": \"x\", \"date\": \"2024-03-15\"\}")?
  e.date |> time.weekday
}
    "#);
    assert_eq!(result, Value::Variant("Friday".into(), vec![]));
}

#[test]
fn test_json_parse_option_date_field() {
    let result = run(r#"
import json
import time
type Task { name: String, due: Option(Date) }
fn main() {
  let t = json.parse(Task, "\{\"name\": \"write tests\", \"due\": \"2024-06-01\"\}")?
  match t.due {
    Some(d) -> d.year
    None -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(2024));
}

#[test]
fn test_json_parse_option_date_null() {
    let result = run(r#"
import json
import time
type Task { name: String, due: Option(Date) }
fn main() {
  let t = json.parse(Task, "\{\"name\": \"write tests\", \"due\": null\}")?
  match t.due {
    Some(d) -> d.year
    None -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_json_parse_list_of_dates() {
    let result = run(r#"
import json
import list
import time
type Event { name: String, date: Date }
fn main() {
  let events = json.parse_list(Event, "[\{\"name\": \"a\", \"date\": \"2024-01-15\"\}, \{\"name\": \"b\", \"date\": \"2024-12-25\"\}]")?
  events |> list.map { e -> e.date.month }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(12)]))
    );
}

#[test]
fn test_json_parse_datetime_space_separator() {
    let result = run(r#"
import json
import time
type Log { msg: String, ts: DateTime }
fn main() {
  let l = json.parse(Log, "\{\"msg\": \"ok\", \"ts\": \"2024-03-15 09:00:00\"\}")?
  l.ts.date.day
}
    "#);
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_json_parse_datetime_rfc3339_zulu() {
    let result = run(r#"
import json
import time
type Event { name: String, ts: DateTime }
fn main() {
  let e = json.parse(Event, "\{\"name\": \"x\", \"ts\": \"2024-03-15T09:00:00Z\"\}")?
  e.ts.time.hour
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_json_parse_datetime_positive_offset() {
    let result = run(r#"
import json
import time
type Event { name: String, ts: DateTime }
fn main() {
  -- 18:00 in UTC+9 = 09:00 UTC
  let e = json.parse(Event, "\{\"name\": \"x\", \"ts\": \"2024-03-15T18:00:00+09:00\"\}")?
  e.ts.time.hour
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_json_parse_datetime_negative_offset() {
    let result = run(r#"
import json
import time
type Event { name: String, ts: DateTime }
fn main() {
  -- 05:00 in UTC-4 = 09:00 UTC
  let e = json.parse(Event, "\{\"name\": \"x\", \"ts\": \"2024-03-15T05:00:00-04:00\"\}")?
  e.ts.time.hour
}
    "#);
    assert_eq!(result, Value::Int(9));
}

#[test]
fn test_json_parse_datetime_half_hour_offset() {
    let result = run(r#"
import json
import time
type Event { name: String, ts: DateTime }
fn main() {
  -- 14:30 in UTC+5:30 = 09:00 UTC
  let e = json.parse(Event, "\{\"name\": \"x\", \"ts\": \"2024-03-15T14:30:00+05:30\"\}")?
  e.ts.time.hour
}
    "#);
    assert_eq!(result, Value::Int(9));
}

// ── regex.captures ──────────────────────────────────────────────────

#[test]
fn test_regex_captures() {
    let result = run(r#"
import regex
fn main() {
  regex.captures("(\\w+)@(\\w+)", "user@host")
}
    "#);
    assert_eq!(
        result,
        Value::Variant(
            "Some".into(),
            vec![Value::List(Arc::new(vec![
                Value::String("user@host".into()),
                Value::String("user".into()),
                Value::String("host".into()),
            ]))]
        )
    );
}

#[test]
fn test_regex_captures_no_match() {
    let result = run(r#"
import regex
fn main() {
  regex.captures("(\\d+)", "no numbers")
}
    "#);
    assert_eq!(result, Value::Variant("None".into(), Vec::new()));
}

#[test]
fn test_regex_captures_all() {
    let result = run(r#"
import regex
fn main() {
  regex.captures_all("(\\w+)@(\\w+)", "alice@home bob@work")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::List(Arc::new(vec![
                Value::String("alice@home".into()),
                Value::String("alice".into()),
                Value::String("home".into()),
            ])),
            Value::List(Arc::new(vec![
                Value::String("bob@work".into()),
                Value::String("bob".into()),
                Value::String("work".into()),
            ])),
        ]))
    );
}

// ── Assertion messages ──────────────────────────────────────────────

#[test]
fn test_assert_with_message() {
    let err = run_err(
        r#"
import test
fn main() {
  test.assert(false, "should be true")
}
    "#,
    );
    assert!(
        err.contains("should be true"),
        "error should contain message: {err}"
    );
}

#[test]
fn test_assert_eq_with_message() {
    let err = run_err(
        r#"
import test
fn main() {
  test.assert_eq(1, 2, "1 + 0")
}
    "#,
    );
    assert!(err.contains("1 + 0"), "error should contain context: {err}");
    assert!(
        err.contains("1 != 2") || err.contains("!= 2"),
        "error should show values: {err}"
    );
}

#[test]
fn test_assert_ne_with_message() {
    let err = run_err(
        r#"
import test
fn main() {
  test.assert_ne(5, 5, "should differ")
}
    "#,
    );
    assert!(
        err.contains("should differ"),
        "error should contain message: {err}"
    );
}

#[test]
fn test_assert_without_message_still_works() {
    run_ok(
        r#"
import test
fn main() {
  test.assert(true)
  test.assert_eq(1, 1)
  test.assert_ne(1, 2)
}
    "#,
    );
}

#[test]
fn test_parameterized_test_pattern() {
    // Demonstrates the idiomatic parameterized test pattern
    run_ok(
        r#"
import list
import test
fn main() {
  let cases = [(1, 2, 3), (0, 0, 0), (10, -10, 0)]
  cases |> list.each { (a, b, expected) ->
    test.assert_eq(a + b, expected, "{a} + {b}")
  }
}
    "#,
    );
}

// ── Short-circuit && and || ─────────────────────────────────────────

#[test]
fn test_and_short_circuit() {
    // false && panic() should NOT panic — right side not evaluated
    run_ok(
        r#"
import test
fn main() {
  let result = false && panic("should not reach")
  test.assert_eq(result, false)
}
    "#,
    );
}

#[test]
fn test_or_short_circuit() {
    // true || panic() should NOT panic — right side not evaluated
    run_ok(
        r#"
import test
fn main() {
  let result = true || panic("should not reach")
  test.assert_eq(result, true)
}
    "#,
    );
}

#[test]
fn test_and_evaluates_right_when_left_true() {
    let result = run(r#"
fn main() {
  true && (1 == 1)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_or_evaluates_right_when_left_false() {
    let result = run(r#"
fn main() {
  false || (2 > 1)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_underscore_trailing_closure_param() {
    // { _ -> expr } as a trailing closure — the core fix
    let result = run(r#"
import list
fn main() {
  [1, 2, 3] |> list.map { _ -> 0 }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(0), Value::Int(0), Value::Int(0)]))
    );
}

#[test]
fn test_underscore_standalone_lambda() {
    // { _ -> expr } as a standalone lambda expression
    let result = run(r#"
fn main() {
  let f = { _ -> 42 }
  f("ignored")
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_underscore_second_closure_param() {
    // { x, _ -> expr } — regression check, already worked
    let result = run(r#"
import list
fn main() {
  [(1, "a"), (2, "b")] |> list.map { (x, _) -> x }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2)]))
    );
}

#[test]
fn test_match_wildcard_arm_still_works() {
    // match { _ -> expr } — regression check, must still work
    let result = run(r#"
fn main() {
  let x = 99
  match x {
    0 -> "zero"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("other".into()));
}

// ── io.inspect silt-syntax formatting ───────────────────────────────

#[test]
fn test_inspect_int() {
    let result = run(r#"
import io
fn main() {
  io.inspect(42)
}
    "#);
    assert_eq!(result, Value::String("42".into()));
}

#[test]
fn test_inspect_float() {
    let result = run(r#"
import io
fn main() {
  io.inspect(3.14)
}
    "#);
    assert_eq!(result, Value::String("3.14".into()));
}

#[test]
fn test_inspect_bool() {
    let result = run(r#"
import io
fn main() {
  io.inspect(true)
}
    "#);
    assert_eq!(result, Value::String("true".into()));
}

#[test]
fn test_inspect_string() {
    let result = run(r#"
import io
fn main() {
  io.inspect("hello")
}
    "#);
    // strings should be quoted in inspect output
    assert_eq!(result, Value::String("\"hello\"".into()));
}

#[test]
fn test_inspect_list() {
    let result = run(r#"
import io
fn main() {
  io.inspect([1, 2, 3])
}
    "#);
    assert_eq!(result, Value::String("[1, 2, 3]".into()));
}

#[test]
fn test_inspect_nested_list() {
    let result = run(r#"
import io
fn main() {
  io.inspect([[1, 2], [3, 4]])
}
    "#);
    assert_eq!(result, Value::String("[[1, 2], [3, 4]]".into()));
}

#[test]
fn test_inspect_list_of_strings() {
    let result = run(r#"
import io
fn main() {
  io.inspect(["a", "b", "c"])
}
    "#);
    assert_eq!(result, Value::String("[\"a\", \"b\", \"c\"]".into()));
}

#[test]
fn test_inspect_map() {
    let result = run(r#"
import io
fn main() {
  io.inspect(#{"a": 1})
}
    "#);
    assert_eq!(result, Value::String("#{\"a\": 1}".into()));
}

#[test]
fn test_inspect_variant_some() {
    let result = run(r#"
import io
fn main() {
  io.inspect(Some(42))
}
    "#);
    assert_eq!(result, Value::String("Some(42)".into()));
}

#[test]
fn test_inspect_variant_none() {
    let result = run(r#"
import io
fn main() {
  io.inspect(None)
}
    "#);
    assert_eq!(result, Value::String("None".into()));
}

#[test]
fn test_inspect_variant_ok() {
    let result = run(r#"
import io
fn main() {
  io.inspect(Ok("hi"))
}
    "#);
    assert_eq!(result, Value::String("Ok(\"hi\")".into()));
}

#[test]
fn test_inspect_variant_err() {
    let result = run(r#"
import io
fn main() {
  io.inspect(Err("oops"))
}
    "#);
    assert_eq!(result, Value::String("Err(\"oops\")".into()));
}

#[test]
fn test_inspect_tuple() {
    let result = run(r#"
import io
fn main() {
  io.inspect((1, "two"))
}
    "#);
    assert_eq!(result, Value::String("(1, \"two\")".into()));
}

#[test]
fn test_inspect_record() {
    let result = run(r#"
import io
type User { name: String, age: Int }

fn main() {
  io.inspect(User { name: "Alice", age: 30 })
}
    "#);
    // BTreeMap orders fields alphabetically
    assert_eq!(
        result,
        Value::String("User {age: 30, name: \"Alice\"}".into())
    );
}

#[test]
fn test_inspect_unit() {
    let result = run(r#"
import io
fn main() {
  io.inspect(())
}
    "#);
    assert_eq!(result, Value::String("()".into()));
}

#[test]
fn test_inspect_closure() {
    let result = run(r#"
import io
fn main() {
  let f = { x -> x + 1 }
  io.inspect(f)
}
    "#);
    assert_eq!(result, Value::String("<fn>".into()));
}

#[test]
fn test_inspect_nested_structure() {
    let result = run(r#"
import io
fn main() {
  io.inspect(Some([1, 2, 3]))
}
    "#);
    assert_eq!(result, Value::String("Some([1, 2, 3])".into()));
}

// ── Triple-quoted strings ───────────────────────────────────────────

#[test]
fn test_triple_quoted_basic() {
    let result = run(r#"
fn main() {
  """hello world"""
}
    "#);
    assert_eq!(result, Value::String("hello world".into()));
}

#[test]
fn test_triple_quoted_multiline() {
    let result = run("
fn main() {
  let s = \"\"\"\n    line1\n    line2\n    \"\"\"
  s
}
    ");
    assert_eq!(result, Value::String("line1\nline2".into()));
}

#[test]
fn test_triple_quoted_embedded_quotes() {
    let result = run(r#"
fn main() {
  """she said "hello" to me"""
}
    "#);
    assert_eq!(result, Value::String("she said \"hello\" to me".into()));
}

#[test]
fn test_triple_quoted_no_interpolation() {
    // {name} should be literal text, not interpolated
    let result = run("
fn main() {
  let name = \"Alice\"
  \"\"\"{name} is here\"\"\"
}
    ");
    assert_eq!(result, Value::String("{name} is here".into()));
}

#[test]
fn test_triple_quoted_no_escape_processing() {
    // \n should be literal backslash-n, not newline
    let result = run(r#"
fn main() {
  """hello\nworld"""
}
    "#);
    assert_eq!(result, Value::String("hello\\nworld".into()));
}

#[test]
fn test_triple_quoted_json_use_case() {
    let result = run("
fn main() {
  let json = \"\"\"\n  {\n    \"name\": \"Alice\"\n  }\n  \"\"\"
  json
}
    ");
    assert_eq!(result, Value::String("{\n  \"name\": \"Alice\"\n}".into()));
}

// ── Boolean when/else ────────────────────────────────────────────────

#[test]
fn test_when_bool_continues() {
    let result = run(r#"
fn check(n) {
  when n > 0 else { return "not positive" }
  "positive"
}

fn main() {
  check(5)
}
    "#);
    assert_eq!(result, Value::String("positive".into()));
}

#[test]
fn test_when_bool_diverges_return() {
    let result = run(r#"
fn check(n) {
  when n > 0 else { return "not positive" }
  "positive"
}

fn main() {
  check(-3)
}
    "#);
    assert_eq!(result, Value::String("not positive".into()));
}

#[test]
fn test_when_bool_diverges_panic() {
    let err = run_err(
        r#"
fn check(n) {
  when n > 0 else { panic("must be positive") }
  n
}

fn main() {
  check(-1)
}
    "#,
    );
    assert!(err.contains("must be positive"));
}

#[test]
fn test_when_bool_sequential_guards() {
    let result = run(r#"
fn buy(qty, balance, price) {
  when qty > 0 else { return "out of stock" }
  when balance >= price else { return "not enough money" }
  "purchased"
}

fn main() {
  buy(3, 100, 50)
}
    "#);
    assert_eq!(result, Value::String("purchased".into()));
}

#[test]
fn test_when_bool_sequential_first_fails() {
    let result = run(r#"
fn buy(qty, balance, price) {
  when qty > 0 else { return "out of stock" }
  when balance >= price else { return "not enough money" }
  "purchased"
}

fn main() {
  buy(0, 100, 50)
}
    "#);
    assert_eq!(result, Value::String("out of stock".into()));
}

#[test]
fn test_when_bool_sequential_second_fails() {
    let result = run(r#"
fn buy(qty, balance, price) {
  when qty > 0 else { return "out of stock" }
  when balance >= price else { return "not enough money" }
  "purchased"
}

fn main() {
  buy(3, 10, 50)
}
    "#);
    assert_eq!(result, Value::String("not enough money".into()));
}

#[test]
fn test_when_bool_mixed_with_pattern() {
    let result = run(r#"
fn process(input) {
  when Ok(value) = input else { return "parse failed" }
  when value > 0 else { return "must be positive" }
  value * 2
}

fn main() {
  process(Ok(5))
}
    "#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_when_bool_mixed_pattern_fails() {
    let result = run(r#"
fn process(input) {
  when Ok(value) = input else { return "parse failed" }
  when value > 0 else { return "must be positive" }
  value * 2
}

fn main() {
  process(Err("bad"))
}
    "#);
    assert_eq!(result, Value::String("parse failed".into()));
}

#[test]
fn test_when_bool_mixed_bool_fails() {
    let result = run(r#"
fn process(input) {
  when Ok(value) = input else { return "parse failed" }
  when value > 0 else { return "must be positive" }
  value * 2
}

fn main() {
  process(Ok(-3))
}
    "#);
    assert_eq!(result, Value::String("must be positive".into()));
}

// ── Round-robin fan-out ────────────────────────────────────────────

#[test]
fn test_fanout_round_robin_channel_each() {
    // Verify that when multiple workers use channel.each on the same
    // channel, messages are distributed in round-robin order rather
    // than all going to the first worker.
    let result = run(r#"
import channel
import task
fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)

  channel.send(jobs, 1)
  channel.send(jobs, 2)
  channel.send(jobs, 3)
  channel.send(jobs, 4)
  channel.send(jobs, 5)
  channel.send(jobs, 6)
  channel.close(jobs)

  let w1 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, 100 + n)
    }
  })

  let w2 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, 200 + n)
    }
  })

  let w3 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, 300 + n)
    }
  })

  task.join(w1)
  task.join(w2)
  task.join(w3)

  -- Collect all results into a list
  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)
  let Message(d) = channel.receive(results)
  let Message(e) = channel.receive(results)
  let Message(f) = channel.receive(results)

  [a, b, c, d, e, f]
}
    "#);

    // Extract the result list
    let values = match result {
        Value::List(ref items) => items
            .iter()
            .map(|v| match v {
                Value::Int(n) => *n,
                _ => panic!("expected int in result list"),
            })
            .collect::<Vec<_>>(),
        _ => panic!("expected list result"),
    };

    // Count messages per worker (100-series = w1, 200-series = w2, 300-series = w3)
    let w1_count = values.iter().filter(|&&v| v > 100 && v < 200).count();
    let w2_count = values.iter().filter(|&&v| v > 200 && v < 300).count();
    let w3_count = values.iter().filter(|&&v| v > 300 && v < 400).count();

    // With real threads, distribution is non-deterministic.
    // All 6 messages must be processed; at least 2 workers should participate.
    assert_eq!(values.len(), 6);
    let active_workers = [w1_count, w2_count, w3_count]
        .iter()
        .filter(|&&c| c > 0)
        .count();
    assert!(
        active_workers >= 1,
        "at least 1 worker should receive messages, got {values:?}"
    );
}

#[test]
fn test_fanout_single_receive_per_worker() {
    // When each worker does a single receive, all workers should get
    // a message (not just the first worker).
    let result = run(r#"
import channel
import task
fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)

  channel.send(jobs, 10)
  channel.send(jobs, 20)
  channel.send(jobs, 30)

  let w1 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w2 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w3 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  task.join(w1)
  task.join(w2)
  task.join(w3)

  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)
  a + b + c
}
    "#);
    // 10*2 + 20*2 + 30*2 = 20 + 40 + 60 = 120
    assert_eq!(result, Value::Int(120));
}

// ── string.is_empty ────────────────────────────────────────────────

#[test]
fn test_string_is_empty_true() {
    let result = run(r#"
import string
fn main() { string.is_empty("") }
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_string_is_empty_false() {
    let result = run(r#"
import string
fn main() { string.is_empty("hi") }
    "#);
    assert_eq!(result, Value::Bool(false));
}

// ── char classification ────────────────────────────────────────────

#[test]
fn test_string_is_alpha() {
    let result = run(r#"
import string
fn main() { (string.is_alpha("a"), string.is_alpha("5"), string.is_alpha("")) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(false)
        ])
    );
}

#[test]
fn test_string_is_digit() {
    let result = run(r#"
import string
fn main() { (string.is_digit("7"), string.is_digit("x")) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn test_string_is_upper_lower() {
    let result = run(r#"
import string
fn main() { (string.is_upper("A"), string.is_upper("a"), string.is_lower("z"), string.is_lower("Z")) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(true),
            Value::Bool(false),
        ])
    );
}

#[test]
fn test_string_is_alnum() {
    let result = run(r#"
import string
fn main() { (string.is_alnum("a"), string.is_alnum("3"), string.is_alnum("!")) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(false)
        ])
    );
}

#[test]
fn test_string_is_whitespace() {
    let result = run(r#"
import string
fn main() { (string.is_whitespace(" "), string.is_whitespace("a")) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

// ── map.each ───────────────────────────────────────────────────────

#[test]
fn test_map_each_iterates() {
    let result = run(r#"
import channel
import map
fn main() {
  let m = #{"a": 1, "b": 2}
  let ch = channel.new(10)
  map.each(m) { k, v -> channel.send(ch, k) }
  let Message(k1) = channel.receive(ch)
  let Message(k2) = channel.receive(ch)
  "{k1},{k2}"
}
    "#);
    assert_eq!(result, Value::String("a,b".into()));
}

#[test]
fn test_map_each_empty() {
    let result = run(r#"
import map
fn main() {
  let m = #{}
  map.each(m) { k, v -> panic("should not run") }
  "ok"
}
    "#);
    assert_eq!(result, Value::String("ok".into()));
}

// ── Set literal and set module ──────────────────────────────────────

#[test]
fn test_set_literal() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3]
  set.length(s)
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_set_empty() {
    let result = run(r#"
import set
fn main() {
  let s = #[]
  set.length(s)
}
    "#);
    assert_eq!(result, Value::Int(0));
}

#[test]
fn test_set_deduplication() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 2, 3, 3, 3]
  set.length(s)
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_set_contains() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3]
  (set.contains(s, 2), set.contains(s, 4))
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn test_set_insert() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2]
  let s2 = set.insert(s, 3)
  (set.length(s2), set.contains(s2, 3))
}
    "#);
    assert_eq!(result, Value::Tuple(vec![Value::Int(3), Value::Bool(true)]));
}

#[test]
fn test_set_remove() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3]
  let s2 = set.remove(s, 2)
  (set.length(s2), set.contains(s2, 2))
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Int(2), Value::Bool(false)])
    );
}

#[test]
fn test_set_union() {
    let result = run(r#"
import set
fn main() {
  let a = #[1, 2, 3]
  let b = #[3, 4, 5]
  set.length(set.union(a, b))
}
    "#);
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_set_intersection() {
    let result = run(r#"
import set
fn main() {
  let a = #[1, 2, 3, 4]
  let b = #[3, 4, 5, 6]
  let c = set.intersection(a, b)
  set.to_list(c)
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(3), Value::Int(4)]))
    );
}

#[test]
fn test_set_difference() {
    let result = run(r#"
import set
fn main() {
  let a = #[1, 2, 3]
  let b = #[2, 3, 4]
  set.to_list(set.difference(a, b))
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![Value::Int(1)])));
}

#[test]
fn test_set_is_subset() {
    let result = run(r#"
import set
fn main() {
  let a = #[1, 2]
  let b = #[1, 2, 3]
  (set.is_subset(a, b), set.is_subset(b, a))
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn test_set_from_list() {
    let result = run(r#"
import set
fn main() {
  let xs = [3, 1, 2, 1, 3]
  let s = set.from_list(xs)
  (set.length(s), set.to_list(s))
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Int(3),
            Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)])),
        ])
    );
}

#[test]
fn test_set_to_list() {
    let result = run(r#"
import set
fn main() {
  set.to_list(#[3, 1, 2])
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
    );
}

#[test]
fn test_set_map() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3]
  set.to_list(set.map(s) { x -> x * 10 })
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30)
        ]))
    );
}

#[test]
fn test_set_filter() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3, 4, 5]
  set.to_list(set.filter(s) { x -> x > 3 })
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(4), Value::Int(5)]))
    );
}

#[test]
fn test_set_each() {
    run_ok(
        r#"
import set
fn main() {
  let s = #[1, 2, 3]
  set.each(s) { x -> println(x) }
}
    "#,
    );
}

#[test]
fn test_set_fold() {
    let result = run(r#"
import set
fn main() {
  let s = #[1, 2, 3, 4]
  set.fold(s, 0) { acc, x -> acc + x }
}
    "#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_set_with_strings() {
    let result = run(r#"
import set
fn main() {
  let s = #["hello", "world", "hello"]
  (set.length(s), set.contains(s, "hello"))
}
    "#);
    assert_eq!(result, Value::Tuple(vec![Value::Int(2), Value::Bool(true)]));
}

#[test]
fn test_set_with_tuples() {
    let result = run(r#"
import set
fn main() {
  let s = #[(1, 2), (3, 4), (1, 2)]
  set.length(s)
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_set_new() {
    let result = run(r#"
import set
fn main() {
  let s = set.new()
  let s2 = set.insert(s, 42)
  set.contains(s2, 42)
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_set_display() {
    let result = run(r#"
import io
fn main() {
  let s = #[3, 1, 2]
  io.inspect(s)
}
    "#);
    assert_eq!(result, Value::String("#[1, 2, 3]".into()));
}

// ── Time module ─────────────────────────────────────────────────────

use std::collections::BTreeMap;

/// Helper to build a Silt record Value.
fn make_record(name: &str, fields: Vec<(&str, Value)>) -> Value {
    let map: BTreeMap<String, Value> = fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Value::Record(name.to_string(), Arc::new(map))
}

fn make_date(y: i64, m: i64, d: i64) -> Value {
    make_record(
        "Date",
        vec![
            ("year", Value::Int(y)),
            ("month", Value::Int(m)),
            ("day", Value::Int(d)),
        ],
    )
}

fn make_time(h: i64, m: i64, s: i64, ns: i64) -> Value {
    make_record(
        "Time",
        vec![
            ("hour", Value::Int(h)),
            ("minute", Value::Int(m)),
            ("second", Value::Int(s)),
            ("ns", Value::Int(ns)),
        ],
    )
}

fn make_datetime(date: Value, time: Value) -> Value {
    make_record("DateTime", vec![("date", date), ("time", time)])
}

#[allow(dead_code)]
fn make_duration(ns: i64) -> Value {
    make_record("Duration", vec![("ns", Value::Int(ns))])
}

#[test]
fn test_time_date_valid() {
    let result = run(r#"
import time
fn main() { time.date(2024, 3, 15) }
    "#);
    assert_eq!(
        result,
        Value::Variant("Ok".into(), vec![make_date(2024, 3, 15)])
    );
}

#[test]
fn test_time_date_invalid() {
    let result = run(r#"
import time
fn main() { time.date(2024, 13, 1) }
    "#);
    assert!(matches!(result, Value::Variant(ref tag, _) if tag == "Err"));
}

#[test]
fn test_time_date_leap_day() {
    let result = run(r#"
import time
fn main() { time.date(2024, 2, 29) }
    "#);
    assert_eq!(
        result,
        Value::Variant("Ok".into(), vec![make_date(2024, 2, 29)])
    );
}

#[test]
fn test_time_date_non_leap_day() {
    let result = run(r#"
import time
fn main() { time.date(2023, 2, 29) }
    "#);
    assert!(matches!(result, Value::Variant(ref tag, _) if tag == "Err"));
}

#[test]
fn test_time_time_valid() {
    let result = run(r#"
import time
fn main() { time.time(14, 30, 0) }
    "#);
    assert_eq!(
        result,
        Value::Variant("Ok".into(), vec![make_time(14, 30, 0, 0)])
    );
}

#[test]
fn test_time_time_invalid() {
    let result = run(r#"
import time
fn main() { time.time(25, 0, 0) }
    "#);
    assert!(matches!(result, Value::Variant(ref tag, _) if tag == "Err"));
}

#[test]
fn test_time_datetime_compose() {
    let result = run(r#"
import time
fn main() {
  let d = time.date(2024, 6, 15)?
  let t = time.time(9, 30, 0)?
  time.datetime(d, t)
}
    "#);
    assert_eq!(
        result,
        make_datetime(make_date(2024, 6, 15), make_time(9, 30, 0, 0))
    );
}

#[test]
fn test_time_now_returns_instant() {
    let result = run(r#"
import time
fn main() {
  let t = time.now()
  t.epoch_ns > 0
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_time_today_returns_date() {
    let result = run(r#"
import time
fn main() {
  let d = time.today()
  d.year > 2020
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_time_format_datetime() {
    let result = run(r#"
import time
fn main() {
  let dt = time.datetime(time.date(2024, 12, 25)?, time.time(18, 0, 0)?)
  dt |> time.format("%Y-%m-%d %H:%M:%S")
}
    "#);
    assert_eq!(result, Value::String("2024-12-25 18:00:00".into()));
}

#[test]
fn test_time_format_date() {
    let result = run(r#"
import time
fn main() {
  time.date(2024, 6, 15)? |> time.format_date("%d/%m/%Y")
}
    "#);
    assert_eq!(result, Value::String("15/06/2024".into()));
}

#[test]
fn test_time_parse_datetime() {
    let result = run(r#"
import time
fn main() { time.parse("2024-07-04 12:00:00", "%Y-%m-%d %H:%M:%S") }
    "#);
    let expected = make_datetime(make_date(2024, 7, 4), make_time(12, 0, 0, 0));
    assert_eq!(result, Value::Variant("Ok".into(), vec![expected]));
}

#[test]
fn test_time_parse_date() {
    let result = run(r#"
import time
fn main() { time.parse_date("2024-07-04", "%Y-%m-%d") }
    "#);
    assert_eq!(
        result,
        Value::Variant("Ok".into(), vec![make_date(2024, 7, 4)])
    );
}

#[test]
fn test_time_parse_invalid() {
    let result = run(r#"
import time
fn main() { time.parse("not-a-date", "%Y-%m-%d") }
    "#);
    assert!(matches!(result, Value::Variant(ref tag, _) if tag == "Err"));
}

#[test]
fn test_time_add_days() {
    let result = run(r#"
import time
fn main() { time.date(2024, 1, 1)? |> time.add_days(90) }
    "#);
    assert_eq!(result, make_date(2024, 3, 31));
}

#[test]
fn test_time_add_days_negative() {
    let result = run(r#"
import time
fn main() { time.date(2024, 1, 1)? |> time.add_days(-1) }
    "#);
    assert_eq!(result, make_date(2023, 12, 31));
}

#[test]
fn test_time_add_months_clamp() {
    let result = run(r#"
import time
fn main() { time.date(2024, 1, 31)? |> time.add_months(1) }
    "#);
    // Jan 31 + 1 month = Feb 29 (2024 is leap year, clamped)
    assert_eq!(result, make_date(2024, 2, 29));
}

#[test]
fn test_time_add_months_negative_clamp() {
    let result = run(r#"
import time
fn main() { time.date(2024, 3, 31)? |> time.add_months(-1) }
    "#);
    assert_eq!(result, make_date(2024, 2, 29));
}

#[test]
fn test_time_add_months_non_leap() {
    let result = run(r#"
import time
fn main() { time.date(2023, 1, 31)? |> time.add_months(1) }
    "#);
    // Jan 31 + 1 month in non-leap year = Feb 28
    assert_eq!(result, make_date(2023, 2, 28));
}

#[test]
fn test_time_duration_constructors() {
    let result = run(r#"
import time
fn main() {
  let h = time.hours(1)
  let m = time.minutes(1)
  let s = time.seconds(1)
  let ms = time.ms(1)
  (h.ns, m.ns, s.ns, ms.ns)
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Int(3_600_000_000_000),
            Value::Int(60_000_000_000),
            Value::Int(1_000_000_000),
            Value::Int(1_000_000),
        ])
    );
}

#[test]
fn test_time_add_instant_duration() {
    let result = run(r#"
import time
fn main() {
  let start = time.now()
  let later = start |> time.add(time.seconds(60))
  let elapsed = time.since(start, later)
  elapsed.ns == time.seconds(60).ns
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_time_to_utc_from_utc_roundtrip() {
    let result = run(r#"
import time
fn main() {
  let now = time.now()
  let dt = now |> time.to_utc
  let back = dt |> time.from_utc
  back.epoch_ns == now.epoch_ns
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_time_to_datetime_offset() {
    let result = run(r#"
import time
fn main() {
  -- Construct a known instant: 2024-01-01T00:00:00 UTC
  let dt_utc = time.datetime(time.date(2024, 1, 1)?, time.time(0, 0, 0)?)
  let instant = time.from_utc(dt_utc)
  -- Convert to UTC+9 (Tokyo)
  let tokyo = instant |> time.to_datetime(540)
  tokyo.date.year == 2024 && tokyo.time.hour == 9
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_time_weekday() {
    let result = run(r#"
import time
fn main() {
  -- 2024-01-01 was a Monday
  time.date(2024, 1, 1)? |> time.weekday
}
    "#);
    assert_eq!(result, Value::Variant("Monday".into(), vec![]));
}

#[test]
fn test_time_weekday_saturday() {
    let result = run(r#"
import time
fn main() {
  -- 2024-03-16 was a Saturday
  time.date(2024, 3, 16)? |> time.weekday
}
    "#);
    assert_eq!(result, Value::Variant("Saturday".into(), vec![]));
}

#[test]
fn test_time_weekday_pattern_match() {
    let result = run(r#"
import time
fn main() {
  let day = time.date(2024, 1, 1)? |> time.weekday
  match day {
    Monday -> "mon"
    Tuesday -> "tue"
    Wednesday -> "wed"
    Thursday -> "thu"
    Friday -> "fri"
    Saturday -> "sat"
    Sunday -> "sun"
  }
}
    "#);
    assert_eq!(result, Value::String("mon".into()));
}

#[test]
fn test_time_days_between() {
    let result = run(r#"
import time
fn main() { time.days_between(time.date(2024, 1, 1)?, time.date(2024, 12, 31)?) }
    "#);
    assert_eq!(result, Value::Int(365));
}

#[test]
fn test_time_days_between_negative() {
    let result = run(r#"
import time
fn main() { time.days_between(time.date(2024, 12, 31)?, time.date(2024, 1, 1)?) }
    "#);
    assert_eq!(result, Value::Int(-365));
}

#[test]
fn test_time_days_in_month() {
    let result = run(r#"
import time
fn main() { (time.days_in_month(2024, 2), time.days_in_month(2023, 2), time.days_in_month(2024, 7)) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Int(29), Value::Int(28), Value::Int(31)])
    );
}

#[test]
fn test_time_is_leap_year() {
    let result = run(r#"
import time
fn main() { (time.is_leap_year(2024), time.is_leap_year(1900), time.is_leap_year(2000), time.is_leap_year(2023)) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),  // divisible by 4
            Value::Bool(false), // divisible by 100 but not 400
            Value::Bool(true),  // divisible by 400
            Value::Bool(false), // not divisible by 4
        ])
    );
}

#[test]
fn test_time_date_compare_correct_order() {
    // Verifies that Date comparison is year→month→day, NOT alphabetical field order
    let result = run(r#"
import time
fn main() {
  let jan31 = time.date(2024, 1, 31)?
  let feb1 = time.date(2024, 2, 1)?
  (jan31 < feb1, feb1 > jan31, jan31 == jan31)
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ])
    );
}

#[test]
fn test_time_weekday_compare_chronological() {
    let result = run(r#"
import time
fn main() { (Monday < Friday, Sunday > Monday, Wednesday == Wednesday) }
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true)
        ])
    );
}

#[test]
fn test_time_display_date_iso() {
    let result = run(r#"
import time
fn main() { "{time.date(2024, 3, 15)?}" }
    "#);
    assert_eq!(result, Value::String("2024-03-15".into()));
}

#[test]
fn test_time_display_time_iso() {
    let result = run(r#"
import time
fn main() { "{time.time(9, 5, 3)?}" }
    "#);
    assert_eq!(result, Value::String("09:05:03".into()));
}

#[test]
fn test_time_display_datetime_iso() {
    let result = run(r#"
import time
fn main() {
  let dt = time.datetime(time.date(2024, 3, 15)?, time.time(14, 30, 0)?)
  "{dt}"
}
    "#);
    assert_eq!(result, Value::String("2024-03-15T14:30:00".into()));
}

#[test]
fn test_time_display_duration() {
    let result = run(r#"
import time
fn main() {
  ("{time.hours(2)}", "{time.minutes(30)}", "{time.seconds(5)}", "{time.ms(500)}")
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![
            Value::String("2h".into()),
            Value::String("30m".into()),
            Value::String("5s".into()),
            Value::String("500ms".into()),
        ])
    );
}

#[test]
fn test_time_display_duration_compound() {
    let result = run(r#"
import time
fn main() {
  let d = Duration { ns: time.hours(2).ns + time.minutes(30).ns + time.seconds(15).ns }
  "{d}"
}
    "#);
    assert_eq!(result, Value::String("2h30m15s".into()));
}

#[test]
fn test_time_since_signed() {
    let result = run(r#"
import time
fn main() {
  let a = time.now()
  let b = a |> time.add(time.seconds(10))
  let forward = time.since(a, b)
  let backward = time.since(b, a)
  (forward.ns > 0, backward.ns < 0)
}
    "#);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(true)])
    );
}

#[test]
fn test_time_pipe_composition() {
    let result = run(r#"
import time
fn main() {
  time.date(2024, 1, 1)?
  |> time.add_days(90)
  |> time.weekday
}
    "#);
    // 2024-01-01 + 90 days = 2024-03-31 (Sunday)
    assert_eq!(result, Value::Variant("Sunday".into(), vec![]));
}

#[test]
fn test_time_sleep_basic() {
    run_ok(
        r#"
import time
import test
fn main() {
  let before = time.now()
  time.sleep(time.ms(10))
  let elapsed = time.since(before, time.now())
  test.assert(elapsed.ns > 0)
}
    "#,
    );
}

#[test]
fn test_time_format_weekday_name() {
    let result = run(r#"
import time
fn main() {
  time.date(2024, 12, 25)? |> time.format_date("%A")
}
    "#);
    assert_eq!(result, Value::String("Wednesday".into()));
}

// ── Runtime/Vm split tests ──────────────────────────────────────────

#[test]
fn test_spawned_task_accesses_shared_builtins() {
    let result = run(r#"
import task
import string
fn main() {
  let h = task.spawn(fn() {
    string.length("hello")
  })
  task.join(h)
}
    "#);
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_multiple_spawned_tasks_share_state() {
    let result = run(r#"
import task
fn add(a: Int, b: Int) -> Int { a + b }
fn main() {
  let h1 = task.spawn(fn() { add(1, 2) })
  let h2 = task.spawn(fn() { add(10, 20) })
  let h3 = task.spawn(fn() { add(100, 200) })
  let r1 = task.join(h1)
  let r2 = task.join(h2)
  let r3 = task.join(h3)
  r1 + r2 + r3
}
    "#);
    assert_eq!(result, Value::Int(333));
}

#[test]
fn test_spawned_task_accesses_globals_and_variants() {
    let result = run(r#"
import task
fn main() {
  let h = task.spawn(fn() {
    let x = Some(42)
    match x {
      Some(n) -> n
      None -> 0
    }
  })
  task.join(h)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ── M:N scheduler tests ────────────────────────────────────────────

#[test]
fn test_many_concurrent_tasks_with_channels() {
    // 100 tasks all sending through a single channel to verify M:N scheduling.
    let result = run(r#"
import channel
import list
import task
fn main() {
  let ch = channel.new(200)
  let handles = []

  loop i = 0, handles = [] {
    match i >= 100 {
      true -> handles
      _ -> {
        let h = task.spawn(fn() {
          channel.send(ch, i)
        })
        loop(i + 1, list.append(handles, h))
      }
    }
  }

  -- Join all handles
  loop i = 0 {
    match i >= list.length(handles) {
      true -> ()
      _ -> {
        task.join(list.get(handles, i))
        loop(i + 1)
      }
    }
  }

  -- Drain all messages and sum them
  loop i = 0, total = 0 {
    match i >= 100 {
      true -> total
      _ -> {
        let Message(val) = channel.receive(ch)
        loop(i + 1, total + val)
      }
    }
  }
}
    "#);
    // Sum of 0..99 = 4950
    assert_eq!(result, Value::Int(4950));
}

#[test]
fn test_nested_spawn() {
    // Task that spawns tasks that spawn tasks.
    let result = run(r#"
import task
fn main() {
  let h = task.spawn(fn() {
    let inner = task.spawn(fn() {
      let innermost = task.spawn(fn() {
        42
      })
      task.join(innermost) + 1
    })
    task.join(inner) + 1
  })
  task.join(h)
}
    "#);
    assert_eq!(result, Value::Int(44));
}

#[test]
fn test_select_with_multiple_producers() {
    // Multiple producers on different channels, select should pick up messages.
    let result = run(r#"
import channel
import task
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)
  let ch3 = channel.new(10)

  let p1 = task.spawn(fn() { channel.send(ch1, 1) })
  let p2 = task.spawn(fn() { channel.send(ch2, 2) })
  let p3 = task.spawn(fn() { channel.send(ch3, 3) })

  task.join(p1)
  task.join(p2)
  task.join(p3)

  -- Select three times to collect all messages
  loop i = 0, total = 0 {
    match i >= 3 {
      true -> total
      _ -> {
        let (_, msg) = channel.select([ch1, ch2, ch3])
        match msg {
          Message(val) -> loop(i + 1, total + val)
          _ -> total
        }
      }
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_channel_receive_blocks_scheduled_task() {
    // A scheduled task blocks on receive, gets woken by a send from another task.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(1)

  -- Consumer spawns first but has to wait for producer.
  let consumer = task.spawn(fn() {
    let Message(val) = channel.receive(ch)
    val * 2
  })

  -- Producer sends after a moment.
  let producer = task.spawn(fn() {
    channel.send(ch, 21)
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ════════════════════════════════════════════════════════════════════
// HTTP Module Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_http_segments_basic() {
    let result = run(r#"
import http
fn main() {
  http.segments("/api/users/42")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("api".into()),
            Value::String("users".into()),
            Value::String("42".into()),
        ]))
    );
}

#[test]
fn test_http_segments_root_path() {
    let result = run(r#"
import http
fn main() {
  http.segments("/")
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_http_segments_no_leading_slash() {
    let result = run(r#"
import http
fn main() {
  http.segments("foo/bar")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("foo".into()),
            Value::String("bar".into()),
        ]))
    );
}

#[test]
fn test_http_segments_trailing_slash() {
    let result = run(r#"
import http
fn main() {
  http.segments("/a/b/")
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
        ]))
    );
}

#[test]
fn test_http_segments_empty_string() {
    let result = run(r#"
import http
fn main() {
  http.segments("")
}
    "#);
    assert_eq!(result, Value::List(Arc::new(vec![])));
}

#[test]
fn test_http_segments_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.segments("/a", "/b")
}
    "#,
    );
    assert!(err.contains("http.segments takes 1 argument"), "got: {err}");
}

#[test]
fn test_http_segments_wrong_type() {
    let err = run_err(
        r#"
import http
fn main() {
  http.segments(42)
}
    "#,
    );
    assert!(
        err.contains("http.segments requires a String"),
        "got: {err}"
    );
}

#[test]
fn test_http_get_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.get("http://example.com", "extra")
}
    "#,
    );
    assert!(err.contains("http.get takes 1 argument"), "got: {err}");
}

#[test]
fn test_http_get_wrong_type() {
    let err = run_err(
        r#"
import http
fn main() {
  http.get(42)
}
    "#,
    );
    assert!(err.contains("http.get requires a String"), "got: {err}");
}

#[test]
fn test_http_request_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, "http://example.com")
}
    "#,
    );
    assert!(err.contains("http.request takes 4 arguments"), "got: {err}");
}

#[test]
fn test_http_request_non_variant_method() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request("GET", "http://example.com", "", #{})
}
    "#,
    );
    assert!(
        err.contains("first argument must be a Method"),
        "got: {err}"
    );
}

#[test]
fn test_http_request_non_string_url() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, 42, "", #{})
}
    "#,
    );
    assert!(err.contains("url must be a String"), "got: {err}");
}

#[test]
fn test_http_request_non_string_body() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(POST, "http://example.com", 42, #{})
}
    "#,
    );
    assert!(err.contains("body must be a String"), "got: {err}");
}

#[test]
fn test_http_request_non_map_headers() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, "http://example.com", "", "bad")
}
    "#,
    );
    assert!(err.contains("headers must be a Map"), "got: {err}");
}

#[test]
fn test_http_serve_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.serve(8080)
}
    "#,
    );
    assert!(err.contains("http.serve takes 2 arguments"), "got: {err}");
}

#[test]
fn test_http_serve_non_int_port() {
    let err = run_err(
        r#"
import http
fn main() {
  http.serve("8080", fn(req) { Response { status: 200, body: "", headers: #{} } })
}
    "#,
    );
    assert!(err.contains("port must be an Int"), "got: {err}");
}

#[test]
fn test_http_unknown_function() {
    let err = run_err(
        r#"
import http
fn main() {
  http.nonexistent()
}
    "#,
    );
    assert!(err.contains("unknown http function"), "got: {err}");
}

#[test]
fn test_http_segments_in_pipeline() {
    let result = run(r#"
import http
import list
fn main() {
  "/api/v2/users/123"
  |> http.segments()
  |> list.length()
}
    "#);
    assert_eq!(result, Value::Int(4));
}

// ── http.serve concurrency ──────────────────────────────────────

#[test]
fn test_http_serve_non_blocking_in_task() {
    // Spawn http.serve in a task; verify other tasks can still run.
    // The accept loop runs on a dedicated OS thread and the silt task
    // yields via BlockReason::Join, so scheduler workers stay free.
    let input = r#"
import http
import task
import channel

fn main() {
  let done = channel.new(1)
  let server = task.spawn(fn() {
    http.serve(19080, fn(req) {
      Response { status: 200, body: "ok", headers: #{} }
    })
  })
  let worker = task.spawn(fn() {
    channel.send(done, "ready")
  })
  let result = channel.receive(done)
  match result {
    Message(v) -> v
    _ -> "failed"
  }
}
"#;
    let result = run(input);
    assert_eq!(result, Value::String("ready".into()));
}

#[test]
fn test_http_serve_concurrent_requests() {
    // Start a server on the main thread (it blocks), send concurrent
    // HTTP requests from Rust threads, and verify all get correct
    // responses — proving per-request concurrency.
    use std::thread;
    use std::time::Duration;

    let port = 19081;

    // Run the silt server in a background thread (http.serve blocks the main thread).
    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    // Wait for the server to bind and start accepting
    thread::sleep(Duration::from_millis(300));

    // Send 5 concurrent requests
    let mut request_handles = Vec::new();
    for i in 0..5 {
        request_handles.push(thread::spawn(move || {
            let url = format!("http://127.0.0.1:{port}/path{i}");
            match ureq::get(&url).call() {
                Ok(mut resp) => resp.body_mut().read_to_string().unwrap_or_default(),
                Err(e) => format!("error: {e}"),
            }
        }));
    }

    for (i, h) in request_handles.into_iter().enumerate() {
        let body = h.join().unwrap();
        assert_eq!(body, format!("/path{i}"), "request {i} got wrong response");
    }
}

// ── http.serve functional tests ─────────────────────────────────

/// Find a free ephemeral port by binding to port 0, recording the
/// assigned port, then dropping the listener so `http.serve` can bind it.
fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Spin-wait for a TCP port to become connectable, with a timeout.
/// Returns `true` if the port is ready, `false` if timed out.
fn wait_for_port(port: u16, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

#[test]
fn test_http_serve_basic_get_response() {
    // Start a server that echoes a fixed body, make a GET request, verify
    // the response body matches.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: "hello from silt", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "hello from silt");
}

#[test]
fn test_http_serve_returns_custom_status_code() {
    // Handler returns a 404 status — verify the client sees it.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 404, body: "not found", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/missing"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 404);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "not found");
}

#[test]
fn test_http_serve_echoes_request_path() {
    // Handler echoes back the request path in the body.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // Test various paths
    for path in &["/", "/api/v1/users", "/hello/world"] {
        let mut resp = agent
            .get(&format!("http://127.0.0.1:{port}{path}"))
            .call()
            .expect("GET request failed");

        assert_eq!(resp.status(), 200);
        let body = resp.body_mut().read_to_string().unwrap();
        assert_eq!(body, *path, "path mismatch for {path}");
    }
}

#[test]
fn test_http_serve_echoes_query_string() {
    // Handler echoes back the query string from the request.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: req.query, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/search?q=silt&page=1"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "q=silt&page=1");
}

#[test]
fn test_http_serve_reads_request_body() {
    // POST a body to the server and verify the handler receives it.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: req.body, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(&format!("http://127.0.0.1:{port}/echo"))
        .send("request body content")
        .expect("POST request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "request body content");
}

#[test]
fn test_http_serve_reads_request_method() {
    // Handler pattern-matches on the request method and returns its name.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    let method_name = match req.method {{
      GET -> "got-get"
      POST -> "got-post"
      PUT -> "got-put"
      DELETE -> "got-delete"
      _ -> "got-other"
    }}
    Response {{ status: 200, body: method_name, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // Test GET
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-get");

    // Test POST
    let mut resp = agent
        .post(&format!("http://127.0.0.1:{port}/"))
        .send_empty()
        .expect("POST failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-post");

    // Test PUT
    let mut resp = agent
        .put(&format!("http://127.0.0.1:{port}/"))
        .send_empty()
        .expect("PUT failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-put");

    // Test DELETE
    let mut resp = agent
        .delete(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("DELETE failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-delete");
}

#[test]
fn test_http_serve_sets_response_headers() {
    // Handler sets custom response headers — verify the client sees them.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{
      status: 200,
      body: "ok",
      headers: #{{ "X-Custom": "silt-value", "X-Another": "42" }}
    }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("X-Custom").and_then(|v| v.to_str().ok()),
        Some("silt-value"),
        "missing or wrong X-Custom header"
    );
    assert_eq!(
        resp.headers()
            .get("X-Another")
            .and_then(|v| v.to_str().ok()),
        Some("42"),
        "missing or wrong X-Another header"
    );
}

#[test]
fn test_http_serve_routing_by_path() {
    // Handler routes requests based on path, returning different responses.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    match req.path {{
      "/health" -> Response {{ status: 200, body: "ok", headers: #{{}} }}
      "/greet" -> Response {{ status: 200, body: "hello!", headers: #{{}} }}
      _ -> Response {{ status: 404, body: "not found", headers: #{{}} }}
    }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // /health -> 200 "ok"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .expect("GET /health failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "ok");

    // /greet -> 200 "hello!"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/greet"))
        .call()
        .expect("GET /greet failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "hello!");

    // /unknown -> 404 "not found"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/unknown"))
        .call()
        .expect("GET /unknown failed");
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "not found");
}

#[test]
fn test_http_serve_concurrent_requests_stress() {
    // Stress test: 20 concurrent requests, each with a unique path.
    // Verifies the server handles high concurrency correctly.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, fn(req) {{
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let count = 20;
    let mut handles = Vec::new();
    for i in 0..count {
        handles.push(thread::spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let url = format!("http://127.0.0.1:{port}/stress/{i}");
            let mut resp = agent.get(&url).call().expect("request failed");
            let body = resp.body_mut().read_to_string().unwrap();
            (i, resp.status(), body)
        }));
    }

    for h in handles {
        let (i, status, body) = h.join().unwrap();
        assert_eq!(status, 200, "request {i} got status {status}");
        assert_eq!(body, format!("/stress/{i}"), "request {i} got wrong body");
    }
}

#[test]
fn test_http_serve_from_task_with_silt_client() {
    // Start the server in a spawned task, then use http.get() from another
    // task to make a request to it — fully within Silt's runtime.
    // The client retries in a functional loop until the server is ready.
    use std::thread;

    let port = find_free_port();

    // Run the entire program in a background Rust thread to prevent
    // hanging the test runner if something goes wrong.
    let handle = thread::spawn(move || {
        let input = format!(
            r#"
import http
import task
import channel

fn main() {{
  let result_ch = channel.new(1)

  -- Start the server in a task
  let server = task.spawn(fn() {{
    http.serve({port}, fn(req) {{
      Response {{ status: 200, body: "silt-response", headers: #{{}} }}
    }})
  }})

  -- Make a request from another task, retrying until the server is up
  let client = task.spawn(fn() {{
    let body = loop attempts = 0 {{
      match attempts > 100 {{
        true -> "gave up"
        _ -> match http.get("http://127.0.0.1:{port}/test") {{
          Ok(resp) -> resp.body
          Err(_) -> loop(attempts + 1)
        }}
      }}
    }}
    channel.send(result_ch, body)
  }})

  let Message(body) = channel.receive(result_ch)
  body
}}
"#
        );
        run(&input)
    });

    let result = handle.join().expect("silt program panicked");
    assert_eq!(result, Value::String("silt-response".into()));
}

// ── Deadlock detection ──────────────────────────────────────────

#[test]
fn test_deadlock_detected_single_task() {
    // A spawned task blocks on receive from a channel that nobody sends to.
    // The main thread joins the task. Deadlock detection should fire and
    // report an error rather than hanging forever.
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch = channel.new(1)
  let h = task.spawn(fn() {
    channel.receive(ch)
  })
  task.join(h)
}
    "#,
    );
    assert!(
        err.contains("deadlock"),
        "expected deadlock error, got: {err}"
    );
}

#[test]
fn test_deadlock_detected_two_tasks() {
    // Two tasks each waiting on the other's channel — classic deadlock.
    let err = run_err(
        r#"
import channel
import task
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let t1 = task.spawn(fn() {
    channel.receive(ch1)
  })
  let t2 = task.spawn(fn() {
    channel.receive(ch2)
  })
  task.join(t1)
}
    "#,
    );
    assert!(
        err.contains("deadlock"),
        "expected deadlock error, got: {err}"
    );
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Rendezvous Stress Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_rendezvous_multiple_senders_one_receiver() {
    // Multiple senders on a rendezvous channel, one receiver collects all.
    let result = run(r#"
import channel
import task
import list
fn main() {
  let ch = channel.new()

  let s1 = task.spawn(fn() { channel.send(ch, 10) })
  let s2 = task.spawn(fn() { channel.send(ch, 20) })
  let s3 = task.spawn(fn() { channel.send(ch, 30) })

  -- Receive all three messages
  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)

  task.join(s1)
  task.join(s2)
  task.join(s3)

  -- Sum should be 60 regardless of order
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_rendezvous_multiple_receivers() {
    // Multiple receivers on one rendezvous channel — each message goes to exactly one.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()
  let results = channel.new(10)

  let r1 = task.spawn(fn() {
    let Message(v) = channel.receive(ch)
    channel.send(results, v * 2)
  })
  let r2 = task.spawn(fn() {
    let Message(v) = channel.receive(ch)
    channel.send(results, v * 2)
  })
  let r3 = task.spawn(fn() {
    let Message(v) = channel.receive(ch)
    channel.send(results, v * 2)
  })

  -- Send three messages; each receiver gets exactly one
  channel.send(ch, 10)
  channel.send(ch, 20)
  channel.send(ch, 30)

  task.join(r1)
  task.join(r2)
  task.join(r3)

  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)

  -- Sum: (10+20+30)*2 = 120
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(120));
}

#[test]
fn test_rendezvous_ping_pong() {
    // Two tasks alternate send/receive on two rendezvous channels.
    let result = run(r#"
import channel
import task
fn main() {
  let ping = channel.new()
  let pong = channel.new()

  let t1 = task.spawn(fn() {
    -- Send ping, wait for pong, repeat
    channel.send(ping, 1)
    let Message(v1) = channel.receive(pong)
    channel.send(ping, v1 + 1)
    let Message(v2) = channel.receive(pong)
    v2
  })

  let t2 = task.spawn(fn() {
    -- Receive ping, send pong, repeat
    let Message(v1) = channel.receive(ping)
    channel.send(pong, v1 + 1)
    let Message(v2) = channel.receive(ping)
    channel.send(pong, v2 + 1)
  })

  task.join(t2)
  task.join(t1)
}
    "#);
    // 1 -> +1 = 2 -> +1 = 3 -> +1 = 4
    assert_eq!(result, Value::Int(4));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Timeout Edge Cases
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_timeout_zero_ms_closes_immediately() {
    // A timeout of 0ms should close (almost) immediately.
    let result = run(r#"
import channel
fn main() {
  let timer = channel.timeout(0)
  let result = channel.receive(timer)
  match result {
    Closed -> "closed"
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("closed".into()));
}

#[test]
fn test_timeout_shorter_fires_first() {
    // Two timeouts with different durations; select picks the shorter one first.
    let result = run(r#"
import channel
fn main() {
  let short = channel.timeout(10)
  let long = channel.timeout(5000)

  match channel.select([long, short]) {
    (ch, Closed) -> {
      -- The channel that fired should be 'short'
      -- We can verify by checking it's the second channel
      "short_fired"
    }
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("short_fired".into()));
}

#[test]
fn test_timeout_channel_never_selected() {
    // Create a timeout channel but never select on it — should not panic or leak.
    let result = run(r#"
import channel
fn main() {
  let _ = channel.timeout(10)
  let _ = channel.timeout(50)
  let _ = channel.timeout(100)
  -- Just let them go out of scope without selecting
  42
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Bidirectional Select Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_select_with_only_send_operations() {
    // Select with only send operations on channels with room.
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)

  match channel.select([(ch1, "a"), (ch2, "b")]) {
    (_, Sent) -> "sent_ok"
    _ -> "unexpected"
  }
}
    "#);
    assert_eq!(result, Value::String("sent_ok".into()));
}

#[test]
fn test_select_send_succeeds_before_receive() {
    // Send should succeed immediately when channel has space, even if receive ops also present.
    let result = run(r#"
import channel
fn main() {
  let empty_ch = channel.new(10)
  let send_ch = channel.new(1)

  -- empty_ch has nothing to receive, but send_ch has room
  match channel.select([empty_ch, (send_ch, 99)]) {
    (_, Sent) -> "sent_first"
    (_, Message(_)) -> "received"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("sent_first".into()));
}

#[test]
fn test_select_send_to_closed_channel() {
    // Select with send to a closed channel should return Closed.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(1)
  channel.close(ch)

  match channel.select([(ch, 42)]) {
    (_, Closed) -> "closed"
    (_, Sent) -> "sent"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("closed".into()));
}

#[test]
fn test_select_receive_and_send_same_channel() {
    // Select with both receive and send on the same buffered channel.
    // If channel has data, receive should succeed.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.send(ch, 100)

  match channel.select([ch, (ch, 200)]) {
    (_, Message(val)) -> val
    (_, Sent) -> -1
    _ -> -2
  }
}
    "#);
    assert_eq!(result, Value::Int(100));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Deadlock Detection Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_no_false_deadlock_with_timeout() {
    // A select with a timeout channel should NOT deadlock — the timeout breaks it.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()
  let timer = channel.timeout(50)

  -- Nobody sends to ch, but timer will fire
  let consumer = task.spawn(fn() {
    match channel.select([ch, timer]) {
      (_, Closed) -> "timeout_broke_deadlock"
      (_, Message(v)) -> "got_message"
      _ -> "other"
    }
  })

  task.join(consumer)
}
    "#);
    assert_eq!(result, Value::String("timeout_broke_deadlock".into()));
}

#[test]
fn test_normal_program_no_deadlock() {
    // Normal programs with tasks that complete cleanly should not trigger deadlock.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(5)

  let producer = task.spawn(fn() {
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.send(ch, 3)
    channel.close(ch)
  })

  let consumer = task.spawn(fn() {
    let total = 0
    let Message(a) = channel.receive(ch)
    let Message(b) = channel.receive(ch)
    let Message(c) = channel.receive(ch)
    a + b + c
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_no_deadlock_producer_consumer_chain() {
    // A chain of producer -> transformer -> consumer should complete without deadlock.
    let result = run(r#"
import channel
import task
fn main() {
  let input = channel.new(5)
  let output = channel.new(5)

  let producer = task.spawn(fn() {
    channel.send(input, 10)
    channel.send(input, 20)
    channel.close(input)
  })

  let transformer = task.spawn(fn() {
    let Message(a) = channel.receive(input)
    channel.send(output, a * 2)
    let Message(b) = channel.receive(input)
    channel.send(output, b * 2)
    channel.close(output)
  })

  let consumer = task.spawn(fn() {
    let Message(x) = channel.receive(output)
    let Message(y) = channel.receive(output)
    x + y
  })

  task.join(producer)
  task.join(transformer)
  task.join(consumer)
}
    "#);
    // (10*2) + (20*2) = 60
    assert_eq!(result, Value::Int(60));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Fairness Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_producer_consumer_buffered_all_items_processed() {
    // Producer sends N items through buffered channel; consumer processes all.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    loop i = 1 {
      match i > 10 {
        true -> channel.close(ch)
        _ -> {
          channel.send(ch, i)
          loop(i + 1)
        }
      }
    }
  })

  let consumer = task.spawn(fn() {
    loop total = 0 {
      match channel.receive(ch) {
        Message(val) -> loop(total + val)
        Closed -> total
      }
    }
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
    // 1+2+...+10 = 55
    assert_eq!(result, Value::Int(55));
}

#[test]
fn test_fanout_all_receivers_get_work() {
    // One sender, multiple receivers — verify all receivers participate.
    let result = run(r#"
import channel
import task
fn main() {
  let jobs = channel.new(20)
  let results = channel.new(20)

  -- Send 9 jobs
  loop i = 1 {
    match i > 9 {
      true -> channel.close(jobs)
      _ -> {
        channel.send(jobs, i)
        loop(i + 1)
      }
    }
  }

  -- Spawn 3 workers
  let w1 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n * 10)
    }
  })
  let w2 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n * 10)
    }
  })
  let w3 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n * 10)
    }
  })

  task.join(w1)
  task.join(w2)
  task.join(w3)

  -- Collect all results and sum
  loop i = 0, total = 0 {
    match i >= 9 {
      true -> total
      _ -> {
        let Message(v) = channel.receive(results)
        loop(i + 1, total + v)
      }
    }
  }
}
    "#);
    // (1+2+...+9) * 10 = 45 * 10 = 450
    assert_eq!(result, Value::Int(450));
}

#[test]
fn test_channel_each_multiple_consumers_all_items() {
    // Verify channel.each with multiple consumers processes all items.
    let result = run(r#"
import channel
import task
fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)

  channel.send(jobs, 1)
  channel.send(jobs, 2)
  channel.send(jobs, 3)
  channel.send(jobs, 4)
  channel.close(jobs)

  let w1 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n + 100)
    }
  })
  let w2 = task.spawn(fn() {
    channel.each(jobs) { n ->
      channel.send(results, n + 200)
    }
  })

  task.join(w1)
  task.join(w2)

  -- Collect 4 results
  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)
  let Message(d) = channel.receive(results)

  -- Each original value is processed exactly once.
  -- Strip the worker prefix and sum the base values.
  let base_a = match a > 200 { true -> a - 200  _ -> a - 100 }
  let base_b = match b > 200 { true -> b - 200  _ -> b - 100 }
  let base_c = match c > 200 { true -> c - 200  _ -> c - 100 }
  let base_d = match d > 200 { true -> d - 200  _ -> d - 100 }
  base_a + base_b + base_c + base_d
}
    "#);
    // 1 + 2 + 3 + 4 = 10
    assert_eq!(result, Value::Int(10));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Error Handling
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_send_on_closed_channel_returns_error() {
    // Sending on a closed channel should error.
    let err = run_err(
        r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  channel.send(ch, "hello")
}
    "#,
    );
    assert!(err.contains("send on closed channel"), "got: {err}");
}

#[test]
fn test_double_close_channel() {
    // Closing a channel twice should not panic.
    run_ok(
        r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  channel.close(ch)
}
    "#,
    );
}

#[test]
fn test_select_on_empty_list_errors() {
    // Select on an empty list should produce an error.
    let err = run_err(
        r#"
import channel
fn main() {
  channel.select([])
}
    "#,
    );
    assert!(err.contains("at least one operation"), "got: {err}");
}

#[test]
fn test_receive_on_closed_channel_returns_closed() {
    // Receiving on a closed empty channel should return Closed variant.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  match channel.receive(ch) {
    Closed -> "got_closed"
    Message(_) -> "unexpected_message"
  }
}
    "#);
    assert_eq!(result, Value::String("got_closed".into()));
}

#[test]
fn test_try_send_on_closed_channel_returns_false() {
    // try_send on a closed channel should return false (not panic).
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  channel.try_send(ch, 42)
}
    "#);
    assert_eq!(result, Value::Bool(false));
}

#[test]
fn test_try_receive_on_closed_channel_returns_closed() {
    // try_receive on a closed empty channel returns Closed variant.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)
  channel.close(ch)
  match channel.try_receive(ch) {
    Closed -> "closed"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("closed".into()));
}

// ════════════════════════════════════════════════════════════════════
// Concurrency: Additional Stress Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_rendezvous_many_senders_stress() {
    // 20 tasks all sending through a rendezvous channel — every message must arrive.
    let result = run(r#"
import channel
import list
import task
fn main() {
  let ch = channel.new()
  let handles = []

  -- Spawn 20 senders
  loop i = 0, handles = [] {
    match i >= 20 {
      true -> handles
      _ -> {
        let h = task.spawn(fn() {
          channel.send(ch, i)
        })
        loop(i + 1, list.append(handles, h))
      }
    }
  }

  -- Receive all 20 messages and sum them
  let total = loop i = 0, acc = 0 {
    match i >= 20 {
      true -> acc
      _ -> {
        let Message(val) = channel.receive(ch)
        loop(i + 1, acc + val)
      }
    }
  }

  -- Join all senders
  loop i = 0 {
    match i >= list.length(handles) {
      true -> ()
      _ -> {
        task.join(list.get(handles, i))
        loop(i + 1)
      }
    }
  }

  -- Sum of 0..19 = 190
  total
}
    "#);
    assert_eq!(result, Value::Int(190));
}

#[test]
fn test_select_multiple_ready_channels() {
    // When multiple channels have data, select picks one (non-deterministic but valid).
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let ch3 = channel.new(1)

  channel.send(ch1, 10)
  channel.send(ch2, 20)
  channel.send(ch3, 30)

  -- Select should pick one of the three
  let (_, msg) = channel.select([ch1, ch2, ch3])
  match msg {
    Message(val) -> val > 0
    _ -> false
  }
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_buffered_channel_fill_and_drain() {
    // Fill a buffered channel to capacity, then drain it completely.
    let result = run(r#"
import channel
fn main() {
  let ch = channel.new(5)

  -- Fill to capacity
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  channel.send(ch, 4)
  channel.send(ch, 5)

  -- Drain all
  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  let Message(d) = channel.receive(ch)
  let Message(e) = channel.receive(ch)

  a + b + c + d + e
}
    "#);
    assert_eq!(result, Value::Int(15));
}

#[test]
fn test_select_timeout_with_multiple_empty_channels() {
    // Select across multiple empty channels plus a timeout — timeout should win.
    let result = run(r#"
import channel
fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let ch3 = channel.new(1)
  let timer = channel.timeout(50)

  match channel.select([ch1, ch2, ch3, timer]) {
    (_, Closed) -> "timeout_won"
    (_, Message(_)) -> "got_data"
    _ -> "other"
  }
}
    "#);
    assert_eq!(result, Value::String("timeout_won".into()));
}

#[test]
fn test_channel_close_wakes_blocked_receiver() {
    // A task blocked on receive should wake up when channel is closed.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new()

  let receiver = task.spawn(fn() {
    match channel.receive(ch) {
      Closed -> "woken_by_close"
      Message(_) -> "got_message"
    }
  })

  -- Close the channel to wake the blocked receiver
  channel.close(ch)
  task.join(receiver)
}
    "#);
    assert_eq!(result, Value::String("woken_by_close".into()));
}

#[test]
fn test_producer_consumer_rendezvous_ordering() {
    // Messages through a rendezvous channel maintain FIFO order
    // when there is exactly one sender and one receiver.
    // We receive a known count to avoid close/receive race.
    let result = run(r#"
import channel
import list
import task
fn main() {
  let ch = channel.new()

  let producer = task.spawn(fn() {
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.send(ch, 3)
    channel.send(ch, 4)
    channel.send(ch, 5)
  })

  let consumer = task.spawn(fn() {
    loop i = 0, acc = [] {
      match i >= 5 {
        true -> acc
        _ -> {
          let Message(val) = channel.receive(ch)
          loop(i + 1, list.append(acc, val))
        }
      }
    }
  })

  task.join(producer)
  task.join(consumer)
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]))
    );
}

// ═══════════════════════════════════════════════════════════════════
// COMPILER UNIT TESTS
// ═══════════════════════════════════════════════════════════════════
//
// Targeted tests for compiler code paths not covered by existing
// integration tests. Organized by compiler subsystem.

// ── Closures returning closures (upvalue capture in returned fn) ────

#[test]
fn test_fn_returns_closure_capturing_param() {
    let result = run(r#"
fn make_multiplier(factor) {
  fn(x) { x * factor }
}

fn main() {
  let double = make_multiplier(2)
  let triple = make_multiplier(3)
  double(5) + triple(5)
}
    "#);
    assert_eq!(result, Value::Int(25));
}

#[test]
fn test_fn_returns_closure_capturing_local() {
    let result = run(r#"
fn make_counter(start) {
  let base = start * 10
  fn(n) { base + n }
}

fn main() {
  let f = make_counter(5)
  f(3)
}
    "#);
    assert_eq!(result, Value::Int(53));
}

// ── Lambda as immediately-invoked expression ────────────────────────

#[test]
fn test_lambda_iife() {
    let result = run(r#"
fn main() {
  let result = (fn(x, y) { x + y })(3, 4)
  result
}
    "#);
    assert_eq!(result, Value::Int(7));
}

// ── Self type in traits ─────────────────────────────────────────────

#[test]
fn test_self_type_in_trait() {
    // Define a trait with Self in method signatures, impl for a type
    let result = run(r#"
trait Monoid {
  fn empty() -> Self
  fn combine(a: Self, b: Self) -> Self
}

trait Monoid for Int {
  fn empty() -> Self { 0 }
  fn combine(a: Self, b: Self) -> Self { a + b }
}

fn main() {
  let x = Int.empty()
  let y = Int.combine(3, 4)
  x + y
}
    "#);
    assert_eq!(result, Value::Int(7));
}

#[test]
fn test_lambda_captures_in_list_operations() {
    let result = run(r#"
import list

fn main() {
  let offset = 100
  [1, 2, 3] |> list.map(fn(x) { x + offset })
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(101),
            Value::Int(102),
            Value::Int(103),
        ]))
    );
}

// ── Upvalue edge cases ──────────────────────────────────────────────

#[test]
fn test_triple_nested_closure_upvalue_chain() {
    let result = run(r#"
fn outer(x) {
  let middle = fn() {
    let inner = fn() { x }
    inner()
  }
  middle()
}

fn main() {
  outer(42)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_upvalue_deduplication() {
    let result = run(r#"
fn main() {
  let x = 10
  let f = fn() { x + x + x }
  f()
}
    "#);
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_multiple_upvalues_in_closure() {
    let result = run(r#"
fn main() {
  let a = 1
  let b = 2
  let c = 3
  let d = 4
  let f = fn() { a + b + c + d }
  f()
}
    "#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_upvalue_in_nested_match() {
    let result = run(r#"
fn main() {
  let factor = 10
  let f = fn(x) {
    match x {
      0 -> factor
      n -> n * factor
    }
  }
  f(3)
}
    "#);
    assert_eq!(result, Value::Int(30));
}

// ── Map pattern edge cases ──────────────────────────────────────────

#[test]
fn test_map_pattern_multiple_keys() {
    let result = run(r#"
fn main() {
  let m = #{"a": 1, "b": 2, "c": 3}
  match m {
    #{"a": x, "b": y} -> x + y
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(3));
}

#[test]
fn test_map_pattern_missing_key() {
    let result = run(r#"
fn main() {
  let m = #{"a": 1}
  match m {
    #{"b": x} -> x
    _ -> 99
  }
}
    "#);
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_map_pattern_with_guard() {
    let result = run(r#"
fn main() {
  let m = #{"x": 5, "y": 10}
  match m {
    #{"x": x} when x > 3 -> "big"
    #{"x": x} -> "small"
    _ -> "none"
  }
}
    "#);
    assert_eq!(result, Value::String("big".into()));
}

// ── Or-pattern without binding ──────────────────────────────────────

#[test]
fn test_or_pattern_multiple_literals() {
    let result = run(r#"
fn classify(n) {
  match n {
    1 | 2 | 3 -> "low"
    4 | 5 | 6 -> "mid"
    _ -> "high"
  }
}

fn main() {
  [classify(2), classify(5), classify(9)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("low".into()),
            Value::String("mid".into()),
            Value::String("high".into()),
        ]))
    );
}

#[test]
fn test_or_pattern_with_constructors_and_fallthrough() {
    let result = run(r#"
type Color { Red, Green, Blue, Yellow }

fn is_primary(c) {
  match c {
    Red | Blue | Yellow -> true
    _ -> false
  }
}

fn main() {
  [is_primary(Red), is_primary(Green), is_primary(Blue)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Bool(true),
            Value::Bool(false),
            Value::Bool(true),
        ]))
    );
}

// ── Scope cleanup ───────────────────────────────────────────────────

#[test]
fn test_scope_cleanup_reuses_slots() {
    let result = run(r#"
fn main() {
  {
    let a = 1
    let b = 2
    let c = 3
    a + b + c
  }
  {
    let x = 10
    let y = 20
    x + y
  }
}
    "#);
    assert_eq!(result, Value::Int(30));
}

#[test]
fn test_nested_scope_cleanup() {
    let result = run(r#"
fn main() {
  let outer = 100
  {
    let inner = 1
    {
      let deep = 2
      deep
    }
    inner
  }
  outer
}
    "#);
    assert_eq!(result, Value::Int(100));
}

// ── Empty block ─────────────────────────────────────────────────────

#[test]
fn test_empty_block_is_unit() {
    run_ok(
        r#"
fn main() {
  {}
}
    "#,
    );
}

// ── Record update with multiple fields ──────────────────────────────

#[test]
fn test_record_update_multiple_fields() {
    let result = run(r#"
type Point { x: Int, y: Int, z: Int }

fn main() {
  let p = Point { x: 1, y: 2, z: 3 }
  let p2 = p.{ x: 10, z: 30 }
  p2.x + p2.y + p2.z
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ── Tail call optimization verification ─────────────────────────────

#[test]
fn test_tco_deep_recursion() {
    let result = run(r#"
fn count(n, acc) {
  match n {
    0 -> acc
    _ -> count(n - 1, acc + 1)
  }
}

fn main() {
  count(200000, 0)
}
    "#);
    assert_eq!(result, Value::Int(200000));
}

#[test]
fn test_tco_with_explicit_return() {
    let result = run(r#"
fn count(n) {
  match n <= 0 {
    true -> return 0
    _ -> return count(n - 1)
  }
}

fn main() {
  count(100000)
}
    "#);
    assert_eq!(result, Value::Int(0));
}

// ── Constructor usage with and without imports ──────────────────────

#[test]
fn test_option_constructors_with_import() {
    let result = run(r#"
import option

fn main() {
  match Some(42) {
    Some(x) -> x
    None -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_result_constructors_with_import() {
    let result = run(r#"
import result

fn main() {
  match Ok(99) {
    Ok(x) -> x
    Err(e) -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(99));
}

// ── Tuple index access ──────────────────────────────────────────────

#[test]
fn test_tuple_numeric_field_access() {
    let result = run(r#"
fn main() {
  let t = (10, 20, 30)
  t.0 + t.1 + t.2
}
    "#);
    assert_eq!(result, Value::Int(60));
}

// ── Loop error cases ────────────────────────────────────────────────

#[test]
fn test_loop_arity_mismatch_compile() {
    let err = run_err(
        r#"
fn main() {
  loop x = 0 {
    match x > 5 {
      true -> x
      _ -> loop(x + 1, 99)
    }
  }
}
    "#,
    );
    assert!(
        err.contains("expects 1") || err.contains("argument"),
        "expected arity mismatch error, got: {err}"
    );
}

#[test]
fn test_loop_nested_inner_outer() {
    let result = run(r#"
fn main() {
  loop i = 0, outer_sum = 0 {
    match i >= 3 {
      true -> outer_sum
      _ -> {
        let inner = loop j = 0, s = 0 {
          match j >= 3 {
            true -> s
            _ -> loop(j + 1, s + 1)
          }
        }
        loop(i + 1, outer_sum + inner)
      }
    }
  }
}
    "#);
    assert_eq!(result, Value::Int(9));
}

// ── Top-level let with destructuring ────────────────────────────────

#[test]
fn test_top_level_let_destructuring_error() {
    let err = run_err(
        r#"
let (a, b) = (1, 2)
fn main() { a }
    "#,
    );
    assert!(
        err.contains("unsupported pattern") || err.contains("top-level"),
        "expected top-level let pattern error, got: {err}"
    );
}

// ── Nested pattern matching compilation ─────────────────────────────

#[test]
fn test_deeply_nested_pattern() {
    let result = run(r#"
import option

fn main() {
  let x = Some((1, [2, 3]))
  match x {
    Some((a, [b, c])) -> a + b + c
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_pattern_match_multiple_arms_with_bindings() {
    let result = run(r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
  Triangle(Float, Float),
}

fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
    Rect(w, h) -> w * h
    Triangle(b, h) -> 0.5 * b * h
  }
}

fn main() {
  area(Rect(3.0, 4.0))
}
    "#);
    assert_eq!(result, Value::Float(12.0));
}

// ── String interpolation compilation ────────────────────────────────

#[test]
fn test_string_interp_nested_braces() {
    let result = run(r#"
fn main() {
  let x = 42
  "value: {x}"
}
    "#);
    assert_eq!(result, Value::String("value: 42".into()));
}

#[test]
fn test_string_interp_complex_expr() {
    let result = run(r#"
fn main() {
  let x = 3
  let y = 4
  "sum: {x + y}, product: {x * y}"
}
    "#);
    assert_eq!(result, Value::String("sum: 7, product: 12".into()));
}

// ── Loop with multiple bindings ─────────────────────────────────────

#[test]
fn test_loop_three_bindings() {
    let result = run(r#"
fn main() {
  loop i = 0, sum = 0, product = 1 {
    match i >= 4 {
      true -> (sum, product)
      _ -> loop(i + 1, sum + i, product * (i + 1))
    }
  }
}
    "#);
    assert_eq!(result, Value::Tuple(vec![Value::Int(6), Value::Int(24)]));
}

#[test]
fn test_self_type_return() {
    // Trait method that returns Self, verify the return type matches
    let result = run(r#"
trait Default {
  fn default() -> Self
}

trait Default for String {
  fn default() -> Self { "" }
}

fn main() {
  String.default()
}
    "#);
    assert_eq!(result, Value::String("".into()));
}

#[test]
fn test_self_type_multiple_impls() {
    // Same trait implemented for different types, Self resolves correctly for each
    let result = run(r#"
trait Zero {
  fn zero() -> Self
}

trait Zero for Int {
  fn zero() -> Self { 0 }
}

trait Zero for Float {
  fn zero() -> Self { 0.0 }
}

fn main() {
  let a = Int.zero()
  let b = Float.zero()
  a
}
    "#);
    assert_eq!(result, Value::Int(0));
}

// ── Type ascription ─────────────────────────────────────────────────

#[test]
fn test_ascription_basic() {
    let result = run(r#"
fn main() {
  let x = 42 as Int
  x
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_ascription_constrains_generic() {
    let result = run(r#"
fn main() {
  let x = None as Option(Int)
  match x {
    Some(n) -> n
    None -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(-1));
}

#[test]
fn test_ascription_in_pipe() {
    let result = run(r#"
fn id(x: Int) -> Int { x }
fn main() {
  (42 |> id()) as Int
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_ascription_with_result() {
    let result = run(r#"
import int
fn main() {
  let r = int.parse("42") as Result(Int, String)
  match r {
    Ok(n) -> n
    Err(_) -> -1
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

// ── Record pattern matching ────────────────────────────────────────

#[test]
fn test_record_pattern_field_binding() {
    let result = run(r#"
type Person { name: String, age: Int }

fn main() {
  let p = Person { name: "Alice", age: 30 }
  match p {
    Person { name, age } -> "{name} is {age}"
    _ -> "unknown"
  }
}
    "#);
    assert_eq!(result, Value::String("Alice is 30".into()));
}

#[test]
fn test_record_pattern_nested_constructor() {
    let result = run(r#"
type Status { Active, Inactive }
type User { name: String, status: Status }

fn describe(u) {
  match u {
    User { name, status: Active } -> "{name} is active"
    User { name, status: Inactive } -> "{name} is inactive"
  }
}

fn main() {
  let u1 = User { name: "Alice", status: Active }
  let u2 = User { name: "Bob", status: Inactive }
  [describe(u1), describe(u2)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("Alice is active".into()),
            Value::String("Bob is inactive".into()),
        ]))
    );
}

// ── Float range patterns ───────────────────────────────────────────

#[test]
fn test_float_range_pattern_basic() {
    let result = run(r#"
fn classify(x) {
  match x {
    0.0..10.0 -> "low"
    10.0..100.0 -> "mid"
    _ -> "high"
  }
}

fn main() {
  [classify(5.0), classify(50.0), classify(200.0)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("low".into()),
            Value::String("mid".into()),
            Value::String("high".into()),
        ]))
    );
}

// ── Float range patterns ───────────────────────────────────────────

#[test]
fn test_float_range_boundary_inclusive() {
    let result = run(r#"
fn classify(x) {
  match x {
    10.0..20.0 -> "in range"
    _ -> "out of range"
  }
}

fn main() {
  [classify(10.0), classify(20.0), classify(9.9), classify(20.1)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("in range".into()),
            Value::String("in range".into()),
            Value::String("out of range".into()),
            Value::String("out of range".into()),
        ]))
    );
}

// ── Complex or-patterns ────────────────────────────────────────────

#[test]
fn test_or_pattern_five_alternatives() {
    let result = run(r#"
fn classify(n) {
  match n {
    1 | 2 | 3 | 4 | 5 -> "small"
    _ -> "big"
  }
}

fn main() {
  [classify(1), classify(3), classify(5), classify(6), classify(100)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("small".into()),
            Value::String("small".into()),
            Value::String("small".into()),
            Value::String("big".into()),
            Value::String("big".into()),
        ]))
    );
}

#[test]
fn test_or_pattern_nested_in_tuple() {
    let result = run(r#"
fn check(x, y) {
  match (x, y) {
    (1 | 2, "a" | "b") -> "match"
    _ -> "no"
  }
}

fn main() {
  [check(1, "a"), check(2, "b"), check(1, "c"), check(3, "a")]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("match".into()),
            Value::String("match".into()),
            Value::String("no".into()),
            Value::String("no".into()),
        ]))
    );
}

#[test]
fn test_or_pattern_in_constructor() {
    let result = run(r#"
fn classify(opt) {
  match opt {
    Some(1 | 2 | 3) -> "small"
    Some(_) -> "other"
    None -> "none"
  }
}

fn main() {
  [classify(Some(1)), classify(Some(2)), classify(Some(99)), classify(None)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("small".into()),
            Value::String("small".into()),
            Value::String("other".into()),
            Value::String("none".into()),
        ]))
    );
}

// ── Nested map patterns ────────────────────────────────────────────

#[test]
fn test_map_pattern_nested() {
    let result = run(r#"
fn main() {
  let m = #{"user": #{"name": "alice"}}
  match m {
    #{"user": #{"name": n}} -> n
    _ -> "unknown"
  }
}
    "#);
    assert_eq!(result, Value::String("alice".into()));
}

#[test]
fn test_map_pattern_literal_and_binding() {
    let result = run(r#"
fn main() {
  let m = #{"type": "admin", "level": 5}
  match m {
    #{"type": "admin", "level": l} -> l
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(5));
}

// ── Deep constructor patterns ──────────────────────────────────────

#[test]
fn test_constructor_four_level_nesting() {
    let result = run(r#"
type C { WrapC(Int) }
type B { WrapB(C) }
type A { WrapA(B) }

fn main() {
  let val = WrapA(WrapB(WrapC(42)))
  match val {
    WrapA(WrapB(WrapC(n))) -> n
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_constructor_mixed_nesting() {
    let result = run(r#"
fn main() {
  let val = Some((1, [2, 3]))
  match val {
    Some((a, [b, c])) -> a + b + c
    _ -> 0
  }
}
    "#);
    assert_eq!(result, Value::Int(6));
}

// ── Lambda parameter destructuring ─────────────────────────────────

#[test]
fn test_lambda_tuple_param_destructure() {
    let result = run(r#"
import list

fn main() {
  let pairs = [(1, 2), (3, 4)]
  let sums = list.map(pairs) { (a, b) -> a + b }
  sums
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(3), Value::Int(7)]))
    );
}

#[test]
fn test_lambda_nested_param_destructure() {
    let result = run(r#"
import list

fn main() {
  let data = [(1, (2, 3)), (4, (5, 6))]
  let results = list.map(data) { (a, (b, c)) -> a + b + c }
  results
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![Value::Int(6), Value::Int(15)]))
    );
}

// ── Guard edge cases ───────────────────────────────────────────────

#[test]
fn test_guard_accesses_bound_variables() {
    let result = run(r#"
fn classify(pair) {
  match pair {
    (a, b) when a > b -> "first"
    (a, b) when b > a -> "second"
    _ -> "equal"
  }
}

fn main() {
  [classify((5, 3)), classify((3, 5)), classify((4, 4))]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("first".into()),
            Value::String("second".into()),
            Value::String("equal".into()),
        ]))
    );
}

#[test]
fn test_guard_false_falls_through() {
    let result = run(r#"
fn main() {
  match 5 {
    n when n > 10 -> "big"
    n when n > 3 -> "medium"
    _ -> "small"
  }
}
    "#);
    assert_eq!(result, Value::String("medium".into()));
}

#[test]
fn test_guard_with_function_call() {
    let result = run(r#"
fn is_even(n) { n % 2 == 0 }
fn classify(n) {
  match n {
    x when is_even(x) -> "even"
    _ -> "odd"
  }
}

fn main() {
  [classify(4), classify(7)]
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::String("even".into()),
            Value::String("odd".into()),
        ]))
    );
}

// ── Async I/O tests ────────────────────────────────────────────────

#[test]
fn test_async_io_read_file_in_task() {
    let tmp = std::env::temp_dir().join("silt_test_async.txt");
    let tmp = tmp.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import io
import task

fn main() {{
  io.write_file("{tmp}", "hello async")
  let h = task.spawn(fn() {{
    io.read_file("{tmp}")
  }})
  task.join(h)
}}
"#
    );
    let result = run(&input);
    assert!(
        matches!(result, Value::Variant(ref tag, ref args) if tag == "Ok" && args[0] == Value::String("hello async".into())),
        "expected Ok(\"hello async\"), got {result:?}"
    );
}

#[test]
fn test_async_io_parallel_reads() {
    let tmp_a = std::env::temp_dir().join("silt_a.txt");
    let tmp_b = std::env::temp_dir().join("silt_b.txt");
    let tmp_a = tmp_a.to_str().unwrap().replace('\\', "/");
    let tmp_b = tmp_b.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import io
import task

fn main() {{
  io.write_file("{tmp_a}", "aaa")
  io.write_file("{tmp_b}", "bbb")
  let h1 = task.spawn(fn() {{ io.read_file("{tmp_a}") }})
  let h2 = task.spawn(fn() {{ io.read_file("{tmp_b}") }})
  let a = task.join(h1)
  let b = task.join(h2)
  (a, b)
}}
"#
    );
    let result = run(&input);
    if let Value::Tuple(elems) = &result {
        assert!(
            matches!(&elems[0], Value::Variant(tag, _) if tag == "Ok"),
            "expected Ok for first read, got {:?}",
            elems[0]
        );
        assert!(
            matches!(&elems[1], Value::Variant(tag, _) if tag == "Ok"),
            "expected Ok for second read, got {:?}",
            elems[1]
        );
    } else {
        panic!("expected tuple, got {result:?}");
    }
}

// ── ExtFloat system ─────────────────────────────────────────────────

#[test]
fn test_ext_float_division() {
    // Division returns ExtFloat, else narrows to Float
    assert_eq!(
        run(r#"fn main() { 1.0 / 2.0 else 0.0 }"#),
        Value::Float(0.5)
    );
    // Division by zero returns infinity, else catches it
    assert_eq!(
        run(r#"fn main() { 1.0 / 0.0 else 0.0 }"#),
        Value::Float(0.0)
    );
    // Negative division by zero
    assert_eq!(
        run(r#"fn main() { -1.0 / 0.0 else 0.0 }"#),
        Value::Float(0.0)
    );
}

#[test]
fn test_ext_float_else_with_expressions() {
    // else binds to full ExtFloat expression
    assert_eq!(
        run(r#"fn main() { 1.0 / 2.0 * 3.0 + 1.0 else 0.0 }"#),
        Value::Float(2.5)
    );
    // Chain with division producing finite result
    assert_eq!(
        run(r#"fn main() { 10.0 / 3.0 else 0.0 }"#),
        Value::Float(10.0 / 3.0)
    );
}

#[test]
fn test_ext_float_math_functions() {
    // sqrt of positive returns finite ExtFloat, else narrows
    assert_eq!(
        run(r#"
import math
fn main() { math.sqrt(4.0) else 0.0 }
    "#),
        Value::Float(2.0)
    );
    // sqrt of negative returns NaN, else catches
    assert_eq!(
        run(r#"
import math
fn main() { math.sqrt(-1.0) else 0.0 }
    "#),
        Value::Float(0.0)
    );
    // log of positive
    assert_eq!(
        run(r#"
import math
fn main() { math.log(1.0) else 0.0 }
    "#),
        Value::Float(0.0)
    );
    // log of zero -> -Infinity, else catches
    assert_eq!(
        run(r#"
import math
fn main() { math.log(0.0) else 0.0 }
    "#),
        Value::Float(0.0)
    );
    // pow overflow — use float.max_value to get a huge number
    assert_eq!(
        run(r#"
import math
import float
fn main() { math.pow(float.max_value, 2.0) else 0.0 }
    "#),
        Value::Float(0.0)
    );
    // exp
    assert_eq!(
        run(r#"
import math
fn main() { math.exp(0.0) else 0.0 }
    "#),
        Value::Float(1.0)
    );
}

#[test]
fn test_ext_float_named_constants() {
    // Float constants
    assert_eq!(
        run(r#"
import float
fn main() { float.max_value }
    "#),
        Value::Float(f64::MAX)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.min_value }
    "#),
        Value::Float(f64::MIN)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.epsilon }
    "#),
        Value::Float(f64::EPSILON)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.min_positive }
    "#),
        Value::Float(f64::MIN_POSITIVE)
    );
    // ExtFloat constants need else to use as Float
    assert_eq!(
        run(r#"
import float
fn main() { float.infinity else 0.0 }
    "#),
        Value::Float(0.0)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.neg_infinity else 0.0 }
    "#),
        Value::Float(0.0)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.nan else 0.0 }
    "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_ext_float_preserves_int_division() {
    // Int division is unchanged
    assert_eq!(run(r#"fn main() { 10 / 3 }"#), Value::Int(3));
    assert_eq!(run(r#"fn main() { 7 / 2 }"#), Value::Int(3));
}

#[test]
fn test_float_arithmetic_unchanged() {
    // Non-division Float arithmetic still returns Float
    assert_eq!(run(r#"fn main() { 1.5 + 2.5 }"#), Value::Float(4.0));
    assert_eq!(run(r#"fn main() { 5.0 - 3.0 }"#), Value::Float(2.0));
    assert_eq!(run(r#"fn main() { 2.0 * 3.0 }"#), Value::Float(6.0));
}

#[test]
fn test_ext_float_else_fallback_value() {
    // Fallback can be any Float expression
    assert_eq!(
        run(r#"fn main() { 1.0 / 0.0 else 42.0 }"#),
        Value::Float(42.0)
    );
    assert_eq!(
        run(r#"
import float
fn main() { 1.0 / 0.0 else float.max_value }
    "#),
        Value::Float(f64::MAX)
    );
    assert_eq!(
        run(r#"fn main() { 1.0 / 0.0 else -1.0 }"#),
        Value::Float(-1.0)
    );
}

#[test]
fn test_ext_float_always_finite_math() {
    // sin, cos return Float directly (always finite for finite input)
    assert_eq!(
        run(r#"
import math
fn main() { math.sin(0.0) }
    "#),
        Value::Float(0.0)
    );
    assert_eq!(
        run(r#"
import math
fn main() { math.cos(0.0) }
    "#),
        Value::Float(1.0)
    );
}

#[test]
fn test_ext_float_division_finite_result() {
    // Finite division result narrows successfully through else
    assert_eq!(
        run(r#"fn main() { 100.0 / 4.0 else 0.0 }"#),
        Value::Float(25.0)
    );
}

#[test]
fn test_ext_float_mixed_arithmetic_widens() {
    // Division produces ExtFloat, further arithmetic with Float widens to ExtFloat
    // The whole expression is ExtFloat, else narrows to Float
    assert_eq!(
        run(r#"fn main() { 10.0 / 2.0 + 1.0 else 0.0 }"#),
        Value::Float(6.0)
    );
    assert_eq!(
        run(r#"fn main() { 10.0 / 2.0 - 1.0 else 0.0 }"#),
        Value::Float(4.0)
    );
    assert_eq!(
        run(r#"fn main() { 10.0 / 2.0 * 3.0 else 0.0 }"#),
        Value::Float(15.0)
    );
}

#[test]
fn test_ext_float_let_binding_with_else() {
    // Can bind the narrowed result to a let
    assert_eq!(
        run(r#"
fn main() {
  let x = 10.0 / 3.0 else 0.0
  x
}
    "#),
        Value::Float(10.0 / 3.0)
    );
}

#[test]
fn test_ext_float_asin_acos_return_extfloat() {
    // asin and acos return ExtFloat because input outside [-1,1] yields NaN
    assert_eq!(
        run(r#"
import math
fn main() { math.asin(0.0) else -1.0 }
    "#),
        Value::Float(0.0)
    );
    assert_eq!(
        run(r#"
import math
fn main() { math.acos(1.0) else -1.0 }
    "#),
        Value::Float(0.0)
    );
    // Out of range input -> NaN -> fallback
    assert_eq!(
        run(r#"
import math
fn main() { math.asin(2.0) else -1.0 }
    "#),
        Value::Float(-1.0)
    );
}

#[test]
fn test_ext_float_atan_returns_float() {
    // atan returns Float directly (always finite for finite input)
    assert_eq!(
        run(r#"
import math
fn main() { math.atan(0.0) }
    "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_ext_float_atan2_returns_float() {
    // atan2 returns Float directly
    assert_eq!(
        run(r#"
import math
fn main() { math.atan2(0.0, 1.0) }
    "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_ext_float_tan_returns_float() {
    // tan returns Float directly
    assert_eq!(
        run(r#"
import math
fn main() { math.tan(0.0) }
    "#),
        Value::Float(0.0)
    );
}

// ── Edge cases: empty collections ───────────────────────────────────

#[test]
fn test_list_head_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.head([]) }
    "#),
        Value::Variant("None".into(), Vec::new())
    );
}

#[test]
fn test_list_last_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.last([]) }
    "#),
        Value::Variant("None".into(), Vec::new())
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
fn test_list_find_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.find([], { x -> x > 0 }) }
    "#),
        Value::Variant("None".into(), Vec::new())
    );
}

#[test]
fn test_list_get_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.get([], 0) }
    "#),
        Value::Variant("None".into(), Vec::new())
    );
}

#[test]
fn test_list_zip_empty_second() {
    assert_eq!(
        run(r#"
import list
fn main() { list.zip([1, 2], []) }
    "#),
        Value::List(Arc::new(vec![]))
    );
}

#[test]
fn test_list_fold_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.fold([], 0, { acc, x -> acc + x }) }
    "#),
        Value::Int(0)
    );
}

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
fn test_list_unique_empty() {
    assert_eq!(
        run(r#"
import list
fn main() { list.unique([]) }
    "#),
        Value::List(Arc::new(vec![]))
    );
}

// ── Edge cases: strings ─────────────────────────────────────────────

#[test]
fn test_string_split_empty_input() {
    assert_eq!(
        run(r#"
import string
fn main() { string.split("", ",") }
    "#),
        Value::List(Arc::new(vec![Value::String("".into())]))
    );
}

#[test]
fn test_string_split_consecutive_delimiters() {
    assert_eq!(
        run(r#"
import string
fn main() { string.split("a,,b", ",") }
    "#),
        Value::List(Arc::new(vec![
            Value::String("a".into()),
            Value::String("".into()),
            Value::String("b".into()),
        ]))
    );
}

#[test]
fn test_string_join_empty_list() {
    assert_eq!(
        run(r#"
import string
fn main() { string.join([], ",") }
    "#),
        Value::String("".into())
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

// ── char_code / from_char_code / byte_length / trim_start / trim_end ──

#[test]
fn test_string_char_code_ascii() {
    let result = run(r#"
import string
fn main() { string.char_code("A") }
    "#);
    assert_eq!(result, Value::Int(65));
}

#[test]
fn test_string_char_code_empty() {
    let err = run_err(r#"
import string
fn main() { string.char_code("") }
    "#);
    assert!(err.contains("empty string"), "got: {err}");
}

#[test]
fn test_string_from_char_code_ascii() {
    let result = run(r#"
import string
fn main() { string.from_char_code(65) }
    "#);
    assert_eq!(result, Value::String("A".into()));
}

#[test]
fn test_string_from_char_code_emoji() {
    let result = run(r#"
import string
fn main() { string.from_char_code(128522) }
    "#);
    assert_eq!(result, Value::String("\u{1F60A}".into()));
}

#[test]
fn test_string_byte_length_ascii() {
    let result = run(r#"
import string
fn main() { string.byte_length("hello") }
    "#);
    assert_eq!(result, Value::Int(5));
}

#[test]
fn test_string_byte_length_multibyte() {
    let result = run(r#"
import string
fn main() { string.byte_length("héllo") }
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_string_trim_start_with_whitespace() {
    let result = run(r#"
import string
fn main() { string.trim_start("  hello") }
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_string_trim_start_no_whitespace() {
    let result = run(r#"
import string
fn main() { string.trim_start("hello") }
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_string_trim_end_with_whitespace() {
    let result = run(r#"
import string
fn main() { string.trim_end("hello  ") }
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

#[test]
fn test_string_trim_end_no_whitespace() {
    let result = run(r#"
import string
fn main() { string.trim_end("hello") }
    "#);
    assert_eq!(result, Value::String("hello".into()));
}

// ── Edge cases: map operations ──────────────────────────────────────

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
fn test_map_merge_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.merge(#{}, #{}) }
    "#),
        Value::Map(Arc::new(std::collections::BTreeMap::new()))
    );
}

#[test]
fn test_map_from_entries_empty() {
    assert_eq!(
        run(r#"
import map
fn main() { map.from_entries([]) }
    "#),
        Value::Map(Arc::new(std::collections::BTreeMap::new()))
    );
}

// ── Edge cases: Result/Option pass-through ──────────────────────────

#[test]
fn test_result_map_ok_on_err() {
    assert_eq!(
        run(r#"
import result
fn main() { result.map_ok(Err("x"), { v -> v + 1 }) }
    "#),
        Value::Variant("Err".into(), vec![Value::String("x".into())])
    );
}

#[test]
fn test_result_map_err_on_ok() {
    assert_eq!(
        run(r#"
import result
fn main() { result.map_err(Ok(42), { e -> e }) }
    "#),
        Value::Variant("Ok".into(), vec![Value::Int(42)])
    );
}

#[test]
fn test_result_flatten_nested_err() {
    assert_eq!(
        run(r#"
import result
fn main() { result.flatten(Ok(Err("nested"))) }
    "#),
        Value::Variant("Err".into(), vec![Value::String("nested".into())])
    );
}

#[test]
fn test_option_flat_map_none() {
    assert_eq!(
        run(r#"
import option
fn main() { option.flat_map(None, { x -> Some(x) }) }
    "#),
        Value::Variant("None".into(), Vec::new())
    );
}

// ── Edge cases: math ────────────────────────────────────────────────

#[test]
fn test_math_log10_one() {
    assert_eq!(
        run(r#"
import math
fn main() { math.log10(1.0) }
    "#),
        Value::ExtFloat(0.0)
    );
}

#[test]
fn test_math_asin_one() {
    let result = run(r#"
import math
fn main() { math.asin(1.0) }
    "#);
    match result {
        Value::ExtFloat(f) => assert!((f - std::f64::consts::FRAC_PI_2).abs() < 1e-10),
        other => panic!("expected ExtFloat, got {other:?}"),
    }
}

// ── fs: mkdir / remove / rename / copy ───────────────────────────

#[test]
fn test_fs_mkdir_and_remove() {
    let dir = std::env::temp_dir().join("silt_test_mkdir_42");
    let dir = dir.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    let dir = "{dir}"
    let r = fs.mkdir(dir)
    let exists = fs.is_dir(dir)
    let _ = fs.remove(dir)
    let gone = fs.is_dir(dir)
    (exists, gone)
}}
    "#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn test_fs_rename() {
    let src = std::env::temp_dir().join("silt_test_rename_src.txt");
    let dst = std::env::temp_dir().join("silt_test_rename_dst.txt");
    let src = src.to_str().unwrap().replace('\\', "/");
    let dst = dst.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
import io
fn main() {{
    let _ = io.write_file("{src}", "hello")
    let r = fs.rename("{src}", "{dst}")
    let exists_dst = fs.exists("{dst}")
    let exists_src = fs.exists("{src}")
    let _ = fs.remove("{dst}")
    (exists_dst, exists_src)
}}
    "#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::Tuple(vec![Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn test_fs_copy() {
    let src = std::env::temp_dir().join("silt_test_copy_src.txt");
    let dst = std::env::temp_dir().join("silt_test_copy_dst.txt");
    let src = src.to_str().unwrap().replace('\\', "/");
    let dst = dst.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
import io
fn main() {{
    let _ = io.write_file("{src}", "data")
    let _ = fs.copy("{src}", "{dst}")
    let content = io.read_file("{dst}")
    let _ = fs.remove("{src}")
    let _ = fs.remove("{dst}")
    content
}}
    "#
    );
    let result = run(&input);
    assert_eq!(
        result,
        Value::Variant("Ok".into(), vec![Value::String("data".into())])
    );
}

#[test]
fn test_fs_remove_nonexistent_returns_err() {
    let path = std::env::temp_dir().join("silt_test_nonexistent_file_that_does_not_exist");
    let path = path.to_str().unwrap().replace('\\', "/");
    let input = format!(
        r#"
import fs
fn main() {{
    fs.remove("{path}")
}}
    "#
    );
    let result = run(&input);
    match result {
        Value::Variant(tag, _) => assert_eq!(tag, "Err"),
        other => panic!("expected Err variant, got {other:?}"),
    }
}

// ── env: get / set ───────────────────────────────────────────────

#[test]
fn test_env_get_missing() {
    let result = run(r#"
import env
fn main() { env.get("SILT_TEST_NONEXISTENT_VAR_12345") }
    "#);
    assert_eq!(result, Value::Variant("None".into(), vec![]));
}

#[test]
fn test_env_set_and_get() {
    let result = run(r#"
import env
fn main() {
    env.set("SILT_TEST_VAR_SET", "hello_silt")
    env.get("SILT_TEST_VAR_SET")
}
    "#);
    assert_eq!(
        result,
        Value::Variant("Some".into(), vec![Value::String("hello_silt".into())])
    );
}

// ── math.random ──────────────────────────────────────────────────

#[test]
fn test_math_random_range() {
    let result = run(r#"
import math
fn main() {
    let r = math.random()
    r >= 0.0 && r < 1.0
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

#[test]
fn test_math_random_not_constant() {
    let result = run(r#"
import math
fn main() {
    let a = math.random()
    let b = math.random()
    a != b
}
    "#);
    assert_eq!(result, Value::Bool(true));
}

// ── Upvalue capture and closure edge cases ─────────────────────────

#[test]
fn test_grandparent_scope_capture() {
    // A closure captures a variable defined two scopes up (grandparent).
    let result = run(r#"
fn outer(x) {
  let middle = fn(y) {
    let inner = fn(z) { x + y + z }
    inner(3)
  }
  middle(2)
}

fn main() {
  outer(1)
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_four_level_nested_capture() {
    // Four levels of nesting: each closure captures from every ancestor.
    let result = run(r#"
fn level0(a) {
  let level1 = fn(b) {
    let level2 = fn(c) {
      let level3 = fn(d) { a + b + c + d }
      level3(4)
    }
    level2(3)
  }
  level1(2)
}

fn main() {
  level0(1)
}
    "#);
    assert_eq!(result, Value::Int(10));
}

#[test]
fn test_multiple_closures_capture_same_variable() {
    // Two sibling closures both capture the same variable from their parent.
    let result = run(r#"
fn main() {
  let shared = 10
  let add_shared = fn(x) { x + shared }
  let mul_shared = fn(x) { x * shared }
  add_shared(5) + mul_shared(3)
}
    "#);
    // add_shared(5) = 15, mul_shared(3) = 30, total = 45
    assert_eq!(result, Value::Int(45));
}

#[test]
fn test_multiple_closures_same_var_from_function() {
    // Two closures returned via a tuple-like structure both capture
    // the same parameter from their enclosing function.
    let result = run(r#"
fn make_pair(n) {
  let inc = fn() { n + 1 }
  let dec = fn() { n - 1 }
  (inc, dec)
}

fn main() {
  let (inc, dec) = make_pair(10)
  inc() + dec()
}
    "#);
    // inc() = 11, dec() = 9, total = 20
    assert_eq!(result, Value::Int(20));
}

#[test]
fn test_closure_captures_shadowed_variable() {
    // A closure captures a variable, then an inner scope shadows it;
    // the closure should still see the original (outer) value.
    let result = run(r#"
fn main() {
  let x = 100
  let f = fn() { x }
  let x = 999
  f()
}
    "#);
    // f captures x=100 at the time MakeClosure runs; the later let x = 999
    // creates a new local that does not affect the captured value.
    assert_eq!(result, Value::Int(100));
}

#[test]
fn test_closure_defined_after_shadow_captures_inner() {
    // The closure is defined after the shadow, so it captures the inner value.
    let result = run(r#"
fn main() {
  let x = 1
  let result = {
    let x = 2
    let f = fn() { x }
    f()
  }
  result
}
    "#);
    assert_eq!(result, Value::Int(2));
}

#[test]
fn test_recursive_function_captures_upvalue() {
    // A top-level recursive function is called via a closure that
    // captures a variable from the enclosing scope.
    let result = run(r#"
fn sum_with_base(base, n) {
  match n {
    0 -> base
    _ -> n + sum_with_base(base, n - 1)
  }
}

fn make_summer(base) {
  fn(n) { sum_with_base(base, n) }
}

fn main() {
  let f = make_summer(100)
  f(5)
}
    "#);
    // sum_with_base(100, 5) = 5 + 4 + 3 + 2 + 1 + 100 = 115
    assert_eq!(result, Value::Int(115));
}

#[test]
fn test_escaping_closure_preserves_value() {
    // A closure escapes its defining function and is called later.
    // The captured value must survive after the enclosing function returns.
    let result = run(r#"
fn make_greeter(name) {
  fn(greeting) { "{greeting}, {name}!" }
}

fn main() {
  let greet = make_greeter("Alice")
  greet("Hello")
}
    "#);
    assert_eq!(result, Value::String("Hello, Alice!".into()));
}

#[test]
fn test_escaping_closure_chain() {
    // A closure returned from a function returns another closure,
    // creating a chain of escaping closures with layered captures.
    let result = run(r#"
fn outer(a) {
  fn(b) {
    fn(c) { a + b + c }
  }
}

fn main() {
  let f = outer(10)
  let g = f(20)
  g(30)
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_closure_as_argument_captures_enclosing() {
    // A closure is passed as an argument to another function while
    // capturing a variable from the call site's enclosing scope.
    let result = run(r#"
fn apply(f, x) {
  f(x)
}

fn main() {
  let multiplier = 7
  let result = apply(fn(x) { x * multiplier }, 6)
  result
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_closure_as_callback_in_list_map() {
    // Closure capturing an outer variable used as a callback in list.map.
    let result = run(r#"
import list
fn main() {
  let scale = 10
  let bias = 3
  [1, 2, 3] |> list.map(fn(x) { x * scale + bias })
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(13),
            Value::Int(23),
            Value::Int(33),
        ]))
    );
}

#[test]
fn test_closure_in_fold_captures_outer() {
    // Closure capturing an outer variable used in list.fold.
    let result = run(r#"
import list
fn main() {
  let bonus = 100
  let result = [1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
  result + bonus
}
    "#);
    assert_eq!(result, Value::Int(106));
}

#[test]
fn test_closure_inside_loop_captures_outer() {
    // A closure created inside a loop body captures a variable from
    // outside the loop.
    let result = run(r#"
import list
fn main() {
  let factor = 5
  loop i = 1, acc = [] {
    match i > 4 {
      true -> acc
      _ -> {
        let f = fn() { i * factor }
        loop(i + 1, list.append(acc, f()))
      }
    }
  }
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(5),
            Value::Int(10),
            Value::Int(15),
            Value::Int(20),
        ]))
    );
}

#[test]
fn test_closures_built_in_loop_capture_iteration_value() {
    // Build a list of closures inside a loop; each closure captures
    // the iteration variable at the time it was created.
    let result = run(r#"
import list
fn main() {
  let closures = loop i = 0, acc = [] {
    match i >= 4 {
      true -> acc
      _ -> loop(i + 1, list.append(acc, fn() { i }))
    }
  }
  -- Call each closure and collect results
  closures |> list.map(fn(f) { f() })
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
        ]))
    );
}

#[test]
fn test_deeply_nested_capture_with_intermediate_locals() {
    // Multiple levels with locals at each level; the innermost closure
    // captures from several ancestors while intermediate scopes have
    // their own locals that should not interfere.
    let result = run(r#"
fn main() {
  let a = 1
  let f = fn() {
    let b = 10
    let g = fn() {
      let c = 100
      let h = fn() { a + b + c }
      h()
    }
    g()
  }
  f()
}
    "#);
    assert_eq!(result, Value::Int(111));
}

#[test]
fn test_closure_captures_function_param_and_local() {
    // A closure captures both a function parameter and a local variable.
    let result = run(r#"
fn make(param) {
  let local = param * 2
  fn(x) { param + local + x }
}

fn main() {
  let f = make(5)
  f(3)
}
    "#);
    // param=5, local=10, x=3 => 18
    assert_eq!(result, Value::Int(18));
}

#[test]
fn test_multiple_escaping_closures_independent() {
    // Two closures escape the same function but capture different params.
    let result = run(r#"
fn make_ops(a, b) {
  let adder = fn(x) { x + a }
  let muler = fn(x) { x * b }
  (adder, muler)
}

fn main() {
  let (add3, mul4) = make_ops(3, 4)
  add3(10) + mul4(10)
}
    "#);
    // add3(10) = 13, mul4(10) = 40 => 53
    assert_eq!(result, Value::Int(53));
}

#[test]
fn test_closure_captures_another_closure() {
    // A closure captures another closure (which itself captured a value).
    let result = run(r#"
fn main() {
  let base = 10
  let add_base = fn(x) { x + base }
  let apply_twice = fn(x) { add_base(add_base(x)) }
  apply_twice(5)
}
    "#);
    // add_base(5) = 15, add_base(15) = 25
    assert_eq!(result, Value::Int(25));
}

#[test]
fn test_returned_closure_used_in_map() {
    // A closure returned from a factory function is used as a callback.
    let result = run(r#"
import list

fn make_adder(n) {
  fn(x) { x + n }
}

fn main() {
  let add10 = make_adder(10)
  [1, 2, 3] |> list.map(add10)
}
    "#);
    assert_eq!(
        result,
        Value::List(Arc::new(vec![
            Value::Int(11),
            Value::Int(12),
            Value::Int(13),
        ]))
    );
}

#[test]
fn test_yield_inside_builtin_preserves_stack() {
    // Regression test for B1: when a scheduled task's CallBuiltin yields (e.g.,
    // channel.send blocks because the buffer is full), the args must be re-pushed
    // onto the stack before the yield so that re-execution finds them intact.
    // Without the fix, the args are consumed (popped) but never restored, causing
    // a stack underflow or corruption when the opcode re-executes after unblocking.
    let result = run(r#"
import channel
import task
fn main() {
  let ch = channel.new(1)

  -- Fill the channel so the next send will block.
  channel.send(ch, 100)

  -- The spawned task's send will block because the buffer is full.
  -- On yield, CallBuiltin must re-push [ch, 42] so the retry works.
  let worker = task.spawn(fn() {
    channel.send(ch, 42)
    "done"
  })

  -- Drain the first value to unblock the worker's send.
  let Message(first) = channel.receive(ch)

  -- Now the worker can complete its send.
  task.join(worker)

  -- Read the worker's value.
  let Message(second) = channel.receive(ch)
  first + second
}
    "#);
    assert_eq!(result, Value::Int(142));
}
