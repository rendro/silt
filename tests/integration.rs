use silt::interpreter::Interpreter;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use std::rc::Rc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    interp.run(&program).expect("runtime error")
}

fn run_ok(input: &str) {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let program = Parser::new(tokens).parse_program().expect("parse error");
    let mut interp = Interpreter::new();
    interp.run(&program).expect("runtime error");
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
  |> map { n -> fizzbuzz(n) }
  |> each { s -> println(s) }
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
  |> map { s -> (s.display(), area(s)) }
  |> each { pair -> println("{pair}") }
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
  |> filter { u -> u.active }
  |> map { u -> birthday(u) }
  |> each { u ->
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

  when Some(host_line) = lines |> find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when Some(port_line) = lines |> find { l -> string.contains(l, "port=") } else {
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
  |> filter { x -> x > 2 }
  |> map { x -> x * 10 }
  |> fold(0) { acc, x -> acc + x }
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
  let ch = chan(10)
  send(ch, 42)
  receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_chan_send_receive_multiple() {
    let result = run(r#"
fn main() {
  let ch = chan(10)
  send(ch, 1)
  send(ch, 2)
  send(ch, 3)
  let a = receive(ch)
  let b = receive(ch)
  let c = receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_spawn_and_join() {
    let result = run(r#"
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, "hello")
    send(ch, "world")
  }

  join(producer)
  let msg1 = receive(ch)
  let msg2 = receive(ch)
  "{msg1} {msg2}"
}
    "#);
    assert_eq!(result, Value::String("hello world".into()));
}

#[test]
fn test_spawn_return_value() {
    let result = run(r#"
fn main() {
  let h = spawn fn() {
    42
  }
  join(h)
}
    "#);
    assert_eq!(result, Value::Int(42));
}

#[test]
fn test_producer_consumer() {
    run_ok(r#"
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, "hello")
    send(ch, "world")
  }

  let consumer = spawn fn() {
    let msg1 = receive(ch)
    let msg2 = receive(ch)
    println("{msg1} {msg2}")
  }

  join(producer)
  join(consumer)
}
    "#);
}

#[test]
fn test_channel_with_integers() {
    let result = run(r#"
fn main() {
  let ch = chan(5)

  let producer = spawn fn() {
    send(ch, 10)
    send(ch, 20)
    send(ch, 30)
  }

  join(producer)

  let a = receive(ch)
  let b = receive(ch)
  let c = receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(60));
}

#[test]
fn test_cancel_task() {
    run_ok(r#"
fn main() {
  let h = spawn fn() {
    42
  }
  cancel(h)
}
    "#);
}

#[test]
fn test_select_expression() {
    let result = run(r#"
fn main() {
  let ch1 = chan(10)
  let ch2 = chan(10)

  send(ch2, "from ch2")

  select {
    receive(ch1) as msg -> "got from ch1"
    receive(ch2) as msg -> msg
  }
}
    "#);
    assert_eq!(result, Value::String("from ch2".into()));
}

#[test]
fn test_select_with_spawn() {
    let result = run(r#"
fn main() {
  let ch1 = chan(10)
  let ch2 = chan(10)

  let p = spawn fn() {
    send(ch1, "first")
  }
  join(p)

  select {
    receive(ch1) as msg -> msg
    receive(ch2) as msg -> msg
  }
}
    "#);
    assert_eq!(result, Value::String("first".into()));
}

#[test]
fn test_unbuffered_channel() {
    let result = run(r#"
fn main() {
  let ch = chan()

  let producer = spawn fn() {
    send(ch, 99)
  }

  join(producer)
  receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(99));
}

#[test]
fn test_multiple_spawns() {
    let result = run(r#"
fn main() {
  let ch = chan(10)

  let h1 = spawn fn() {
    send(ch, 1)
  }

  let h2 = spawn fn() {
    send(ch, 2)
  }

  let h3 = spawn fn() {
    send(ch, 3)
  }

  join(h1)
  join(h2)
  join(h3)

  let a = receive(ch)
  let b = receive(ch)
  let c = receive(ch)
  a + b + c
}
    "#);
    assert_eq!(result, Value::Int(6));
}

#[test]
fn test_channel_passing_complex_values() {
    let result = run(r#"
fn main() {
  let ch = chan(5)
  send(ch, [1, 2, 3])
  let list = receive(ch)
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
  let ch = chan(10)

  let h = spawn fn() {
    send(ch, x * 2)
  }

  join(h)
  receive(ch)
}
    "#);
    assert_eq!(result, Value::Int(20));
}
