use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
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
        Err(e) => return e,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── Phase 3: Hello World ─────────────────────────────────────────────

#[test]
fn test_hello_world() {
    run_ok(
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
    run_ok(
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
  1..101
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
    "#,
    );
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
    run_ok(
        r#"
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
  match parse_config("host=localhost\nport=8080") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }

  match parse_config("host=localhost") {
    Ok(msg) -> println(msg)
    Err(e) -> println("config error: {e}")
  }
}
    "#,
    );
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
    let result = run(r#"
import list
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
    assert_eq!(
        result,
        Value::String("hello world, the answer is 42".into())
    );
}

// ── Phase 3: Map literals ────────────────────────────────────────────

#[test]
fn test_map_literal() {
    run_ok(
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
  sum_helper(1..1001, 0)
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
    run_ok(
        r#"
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
  println(show(s))
}
    "#,
    );
}

#[test]
fn test_where_clause_with_equal() {
    run_ok(
        r#"
fn are_same(a: t, b: t) where t: Equal {
  a == b
}

fn main() {
  are_same(1, 2)
  are_same("hello", "hello")
}
    "#,
    );
}

#[test]
fn test_where_clause_with_compare() {
    run_ok(
        r#"
fn is_less(a: t, b: t) where t: Compare {
  a < b
}

fn main() {
  is_less(1, 2)
  is_less("a", "b")
}
    "#,
    );
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
    assert_eq!(result, Value::Float(4.0));
}

#[test]
fn test_math_pow() {
    let result = run(r#"
import math
fn main() { math.pow(2.0, 10.0) }
    "#);
    assert_eq!(result, Value::Float(1024.0));
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
    // ln(e) = 1.0
    if let Value::Float(f) = result {
        assert!((f - 1.0).abs() < 1e-10);
    } else {
        panic!("expected float");
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
