use super::test_helpers::*;
use super::*;

// ── Basic type inference ────────────────────────────────────────

#[test]
fn test_int_literal() {
    assert_no_errors(
        r#"
fn main() {
  let x = 42
  let _ = x
}
        "#,
    );
}

#[test]
fn test_float_literal() {
    assert_no_errors(
        r#"
fn main() {
  let x = 3.14
  let _ = x
}
        "#,
    );
}

#[test]
fn test_string_literal() {
    assert_no_errors(
        r#"
fn main() {
  let x = "hello"
  let _ = x
}
        "#,
    );
}

#[test]
fn test_bool_literal() {
    assert_no_errors(
        r#"
fn main() {
  let x = true
  let _ = x
}
        "#,
    );
}

#[test]
fn test_arithmetic() {
    assert_no_errors(
        r#"
fn main() {
  let x = 1 + 2
  let y = x * 3
  let _ = y
}
        "#,
    );
}

#[test]
fn test_comparison() {
    assert_no_errors(
        r#"
fn main() {
  let x = 1 < 2
  let _ = x
}
        "#,
    );
}

#[test]
fn test_function_call() {
    assert_no_errors(
        r#"
fn add(a, b) {
  a + b
}

fn main() {
  let _ = add(1, 2)
}
        "#,
    );
}

#[test]
fn test_shadowing() {
    assert_no_errors(
        r#"
fn main() {
  let x = 1
  let x = x + 1
  let x = x * 3
  let _ = x
}
        "#,
    );
}

// ── List inference ──────────────────────────────────────────────

#[test]
fn test_list_inference() {
    assert_no_errors(
        r#"
fn main() {
  let xs = [1, 2, 3]
  let _ = xs
}
        "#,
    );
}

#[test]
fn test_empty_list() {
    assert_no_errors(
        r#"
fn main() {
  let xs = []
  let _ = xs
}
        "#,
    );
}

// ── Tuple inference ─────────────────────────────────────────────

#[test]
fn test_tuple_inference() {
    assert_no_errors(
        r#"
fn main() {
  let pair = (1, "hello")
  let _ = pair
}
        "#,
    );
}

// ── Lambda inference ────────────────────────────────────────────

#[test]
fn test_lambda() {
    assert_no_errors(
        r#"
fn main() {
  let double = { x -> x * 2 }
  let _ = double(5)
}
        "#,
    );
}

// ── Enum types ──────────────────────────────────────────────────

#[test]
fn test_enum_type() {
    assert_no_errors(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn area(shape) {
  match shape {
    Circle(r) -> 3.14159 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  let _ = area(Circle(5.0))
}
        "#,
    );
}

// ── Record types ────────────────────────────────────────────────

#[test]
fn test_record_type() {
    assert_no_errors(
        r#"
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn main() {
  let u = User { name: "Alice", age: 30, active: true }
  let _ = u.name
}
        "#,
    );
}

#[test]
fn test_record_update() {
    assert_no_errors(
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
  let _ = u2.age
}
        "#,
    );
}

// ── Match exhaustiveness ────────────────────────────────────────

#[test]
fn test_match_exhaustive_with_wildcard() {
    assert_no_errors(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    _ -> "other"
  }
}

fn main() {
  let _ = describe(Circle(1.0))
}
        "#,
    );
}

#[test]
fn test_match_exhaustive_all_variants() {
    assert_no_errors(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

fn describe(shape) {
  match shape {
    Circle(r) -> "circle"
    Rect(w, h) -> "rect"
  }
}

fn main() {
  let _ = describe(Circle(1.0))
}
        "#,
    );
}

#[test]
fn test_match_non_exhaustive() {
    assert_has_error(
        r#"
type Color {
  Red,
  Green,
  Blue,
}

fn name(c) {
  match c {
    Red -> "red"
    Green -> "green"
  }
}

fn main() {
  let _ = name(Red)
}
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_non_exhaustive_nested_option() {
    // The new Maranget algorithm catches nested patterns.
    // Matching Ok(Some(x)) and Err(e) misses Ok(None).
    assert_has_error(
        r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Err(e) -> 0
  }
}
fn main() { let _ = handle(Ok(Some(1))) }
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_exhaustive_nested_option() {
    // Full coverage of nested Option inside Result.
    assert_no_errors(
        r#"
fn handle(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> 0
    Err(e) -> 0
  }
}
fn main() { let _ = handle(Ok(Some(1))) }
        "#,
    );
}

#[test]
fn test_match_non_exhaustive_bool_in_tuple() {
    // Tuple of bools: (true, true) and (false, false) misses mixed cases.
    assert_has_error(
        r#"
fn check(pair) {
  match pair {
    (true, true) -> "both"
    (false, false) -> "neither"
  }
}
fn main() { let _ = check((true, true)) }
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_exhaustive_bool_tuple() {
    assert_no_errors(
        r#"
fn check(pair) {
  match pair {
    (true, true) -> "both true"
    (true, false) -> "first true"
    (false, _) -> "first false"
  }
}
fn main() { let _ = check((true, true)) }
        "#,
    );
}

// ── Generic types ───────────────────────────────────────────────

#[test]
fn test_option_some_none() {
    assert_no_errors(
        r#"
fn main() {
  let x = Some(42)
  let y = None
  let _ = match x {
    Some(n) -> n
    None -> 0
  }
}
        "#,
    );
}

#[test]
fn test_result_ok_err() {
    assert_no_errors(
        r#"
fn main() {
  let x = Ok(42)
  let _ = match x {
    Ok(n) -> n
    Err(e) -> 0
  }
}
        "#,
    );
}

// ── Question mark operator ──────────────────────────────────────

#[test]
fn test_question_mark() {
    assert_no_errors(
        r#"
fn process(x) {
  let val = Ok(x)?
  Ok(val * 2)
}

fn main() {
  let _ = match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
    );
}

// ── When guard (type narrowing) ─────────────────────────────────

#[test]
fn test_when_guard() {
    assert_no_errors(
        r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  Ok(value * 2)
}

fn main() {
  let _ = match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
    );
}

// ── Boolean when guard ────────────────────────────────────────────

#[test]
fn test_when_bool_guard() {
    assert_no_errors(
        r#"
fn check(n) {
  when n > 0 else {
    return "not positive"
  }
  "positive"
}

fn main() {
  let _ = check(5)
}
        "#,
    );
}

#[test]
fn test_when_bool_mixed_with_pattern_guard() {
    assert_no_errors(
        r#"
fn process(x) {
  when let Ok(value) = Ok(x) else {
    return Err("failed")
  }
  when value > 0 else {
    return Err("must be positive")
  }
  Ok(value * 2)
}

fn main() {
  let _ = match process(21) {
    Ok(n) -> n
    Err(_) -> 0
  }
}
        "#,
    );
}

// ── Pipe operator ───────────────────────────────────────────────

#[test]
fn test_pipe_operator() {
    assert_no_errors(
        r#"
import list
fn main() {
  let _ = [1, 2, 3, 4, 5]
  |> list.filter { x -> x > 2 }
  |> list.map { x -> x * 10 }
  |> list.fold(0) { acc, x -> acc + x }
}
        "#,
    );
}

// ── String interpolation ────────────────────────────────────────

#[test]
fn test_string_interpolation() {
    assert_no_errors(
        r#"
fn main() {
  let name = "world"
  let n = 42
  let _ = "hello {name}, the answer is {n}"
}
        "#,
    );
}

// ── Trait implementation ────────────────────────────────────────

#[test]
fn test_trait_impl() {
    assert_no_errors(
        r#"
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

fn main() {
  let s = Circle(5.0)
  let _ = "{s}"
}
        "#,
    );
}

// ── Map literal ─────────────────────────────────────────────────

#[test]
fn test_map_literal() {
    assert_no_errors(
        r#"
fn main() {
  let m = #{ "name": "Alice", "age": "30" }
  let _ = m
}
        "#,
    );
}

// ── Integration test programs ───────────────────────────────────

#[test]
fn test_fizzbuzz_program() {
    assert_no_errors(
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
  let _ = results
}
        "#,
    );
}

#[test]
fn test_closures_and_higher_order() {
    assert_no_errors(
        r#"
fn make_adder(n) {
  { x -> x + n }
}

fn main() {
  let add5 = make_adder(5)
  let _ = add5(10)
}
        "#,
    );
}

#[test]
fn test_error_handling_pipeline() {
    assert_no_errors(
        r#"
import list
import string
import int
fn parse_config(text) {
  let lines = text |> string.split("\n")

  when let Some(host_line) = lines |> list.find { l -> string.contains(l, "host=") } else {
    return Err("missing host in config")
  }

  when let Some(port_line) = lines |> list.find { l -> string.contains(l, "port=") } else {
    return Err("missing port in config")
  }

  let host = host_line |> string.replace("host=", "")
  let port_result = port_line |> string.replace("port=", "") |> int.parse()
  when let Ok(port) = port_result else {
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

#[test]
fn test_match_with_guards() {
    assert_no_errors(
        r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  let _ = [classify(-5), classify(0), classify(42)]
}
        "#,
    );
}

// ── Let-polymorphism ────────────────────────────────────────────

#[test]
fn test_let_polymorphism() {
    assert_no_errors(
        r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  let _ = a
}
        "#,
    );
}

// ── Unification error ───────────────────────────────────────────

#[test]
fn test_type_mismatch_in_binary_op() {
    assert_has_error(
        r#"
fn main() {
  let x = 42 + 1.5
  x
}
            "#,
        "type mismatch",
    );
}

#[test]
fn test_bool_op_type_mismatch() {
    assert_has_error(
        r#"
fn main() {
  let x = 42 && true
  let _ = x
}
            "#,
        "type mismatch",
    );
}

// ── Range ───────────────────────────────────────────────────────

#[test]
fn test_range_expression() {
    assert_no_errors(
        r#"
fn main() {
  let r = 1..10
  let _ = r
}
        "#,
    );
}

// ── Exhaustiveness: guards don't count as covering ──────────────

#[test]
fn test_match_guards_with_catch_all() {
    assert_no_errors(
        r#"
fn classify(n) {
  match n {
    0 -> "zero"
    x when x > 0 -> "positive"
    _ -> "negative"
  }
}

fn main() {
  let _ = classify(5)
}
        "#,
    );
}

// ── Severity tests ─────────────────────────────────────────────

#[test]
fn test_type_error_has_error_severity() {
    // A type mismatch should produce Severity::Error.
    //
    // The lock is on the specific Int/String mismatch under test, not on
    // *some* error with Error severity (any unrelated diagnostic would
    // satisfy that): find the diagnostic whose message mentions both
    // "Int" and "String" and assert IT has Error severity, so the test
    // fails if the mismatch becomes a Warning.
    let errors = check_errors(
        r#"
            fn main() {
                let x: Int = "hello"
                let _ = x
            }
        "#,
    );
    assert!(!errors.is_empty());
    let mismatch = errors
        .iter()
        .find(|e| e.message.contains("Int") && e.message.contains("String"))
        .unwrap_or_else(|| {
            panic!(
                "expected an error mentioning both Int and String, got: {:?}",
                errors.iter().map(|e| &e.message).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        mismatch.severity,
        Severity::Error,
        "Int/String mismatch must be Error severity, got {:?} for message {:?}",
        mismatch.severity,
        mismatch.message
    );
}

#[test]
fn test_valid_program_no_errors() {
    let errors = check_errors(
        r#"
            fn main() {
                let x = 42
                let _ = x + 1
            }
        "#,
    );
    let hard_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.severity == Severity::Error)
        .collect();
    assert!(hard_errors.is_empty());
}

#[test]
fn test_trait_impl_validates_methods() {
    // Complete impl should have no errors about missing methods
    let errors = check_errors(
        r#"
            trait Greet {
                fn greet(self) -> String {
                    "hello"
                }
            }
            trait Greet for User {
                fn greet(self) -> String {
                    "hi"
                }
            }
            type User { name: String }
            fn main() { let _ = 0 }
        "#,
    );
    let trait_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.message.contains("missing method"))
        .collect();
    assert!(
        trait_errors.is_empty(),
        "unexpected trait errors: {:?}",
        trait_errors
    );
}

#[test]
fn test_trait_impl_missing_method() {
    // Both trait methods are abstract (no body) so omitting `detail`
    // in the impl is genuinely missing — not silently filled in by a
    // default: a method with a body would be the impl's too, and not
    // reported.
    let errors = check_errors(
        r#"
            trait Showable {
                fn show(self) -> String
                fn detail(self) -> String
            }
            trait Showable for Item {
                fn show(self) -> String { "item" }
            }
            type Item { name: String }
            fn main() { let _ = 0 }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("missing method") && e.message.contains("detail"))
    );
}

#[test]
fn test_trait_impl_unknown_trait() {
    let errors = check_errors(
        r#"
            trait Nonexistent for Thing {
                fn foo(self) -> Int { 0 }
            }
            type Thing { x: Int }
            fn main() { let _ = 0 }
        "#,
    );
    assert!(errors.iter().any(|e| e.message.contains("not declared")));
}

#[test]
fn test_builtin_display_trait_exists() {
    // Implementing Display should not produce "trait not declared" error
    let errors = check_errors(
        r#"
            type Color { Red, Blue }
            trait Display for Color {
                fn display(self) -> String {
                    match self {
                        Red -> "red"
                        Blue -> "blue"
                    }
                }
            }
            fn main() { let _ = 0 }
        "#,
    );
    let undeclared: Vec<_> = errors
        .iter()
        .filter(|e| e.message.contains("not declared"))
        .collect();
    assert!(
        undeclared.is_empty(),
        "Display should be a built-in trait: {:?}",
        undeclared
    );
}

#[test]
fn test_where_unknown_trait_warning() {
    let errors = check_errors(
        r#"
            fn show(x) where x: Nonexistent {
                x
            }
            fn main() { let _ = 0 }
        "#,
    );
    assert!(errors.iter().any(|e| e.message.contains("Nonexistent")));
}

#[test]
fn test_where_constraint_satisfied() {
    // Should produce no errors about constraints
    let errors = check_errors(
        r#"
            trait Showable {
                fn show(self) -> String { "default" }
            }
            type Color { Red, Blue }
            trait Showable for Color {
                fn show(self) -> String { "color" }
            }
            fn display(x) where x: Showable {
                x
            }
            fn main() {
                let _ = display(Red)
            }
        "#,
    );
    let constraint_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.message.contains("does not implement"))
        .collect();
    assert!(
        constraint_errors.is_empty(),
        "unexpected: {:?}",
        constraint_errors
    );
}

#[test]
fn test_where_constraint_violated() {
    // Pin the real path (a `where x: Showable` on a value `x`, not a type
    // variable, would only reach the constraint-introduction check,
    // whose suggestion also contains "Showable"): declare `display` with a proper typed
    // parameter `x: a where a: Showable`, implement Showable for Int
    // only, then call `display("text")`. Int satisfies; String does
    // not. Must produce "type 'String' does not implement trait
    // 'Showable'".
    let errors = check_errors(
        r#"
            trait Showable { fn show(self) -> String }
            trait Showable for Int { fn show(self) -> String { "int" } }
            fn display(x: a) -> String where a: Showable { x.show() }
            fn main() { let _ = display("text") }
        "#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("does not implement") && e.message.contains("Showable")),
        "expected 'does not implement trait Showable', got: {errors:?}"
    );
}

// ── Record types with generic fields (List, Map) ───────────────

#[test]
fn test_record_with_list_field() {
    assert_no_errors(
        r#"
type Bag {
  items: List(Int),
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  let _ = b.name
}
        "#,
    );
}

#[test]
fn test_record_with_map_field() {
    assert_no_errors(
        r#"
type Config {
  data: Map(String, String),
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  let _ = c.data
}
        "#,
    );
}

#[test]
fn test_record_with_list_and_map_fields() {
    assert_no_errors(
        r#"
type Config {
  values: Map(String, Int),
  errors: List(String),
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  let _ = c.values
}
        "#,
    );
}

#[test]
fn test_record_with_list_field_access() {
    assert_no_errors(
        r#"
type Bag {
  items: List(Int),
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  let _ = b.items
}
        "#,
    );
}

// ── Tests for newly registered builtins ────────────────────────

#[test]
fn test_list_module_builtins() {
    assert_no_errors(
        r#"
import list
fn main() {
  let xs = [1, 2, 3]
  let ys = list.append(xs, 4)
  let zs = list.concat(xs, ys)
  let head = list.first(xs)
  let tail = list.tail(xs)
  let last = list.last(xs)
  let rev = list.reverse(xs)
  let sorted = list.sort(xs)
  let has = list.contains(xs, 2)
  let n = list.length(xs)
  let taken = list.take(xs, 2)
  let dropped = list.drop(xs, 1)
  let got = list.get(xs, 0)
  let pairs = list.enumerate(xs)
  let _ = n
}
        "#,
    );
}

#[test]
fn test_string_module_builtins() {
    assert_no_errors(
        r#"
import string
fn main() {
  let s = "hello world"
  let upper = string.to_upper(s)
  let lower = string.to_lower(s)
  let n = string.length(s)
  let starts = string.starts_with(s, "hello")
  let ends = string.ends_with(s, "world")
  let chars = string.chars(s)
  let repeated = string.repeat(s, 3)
  let idx = string.index_of(s, "world")
  let sliced = string.slice(s, 0, 5)
  let replaced = string.replace(s, "world", "there")
  let _ = n
}
        "#,
    );
}

#[test]
fn test_float_module_builtins() {
    assert_no_errors(
        r#"
import float
fn main() {
  let a = 3.14
  let b = 2.71
  let mn = float.min(a, b)
  let mx = float.max(a, b)
  let parsed = float.parse("3.14")
  let rounded = float.round(a)
  let ceiled = float.ceil(a)
  let floored = float.floor(a)
  let abs = float.abs(a)
  let _ = rounded
}
        "#,
    );
}

#[test]
fn test_int_module_builtins() {
    assert_no_errors(
        r#"
import int
fn main() {
  let a = 5
  let b = 3
  let mn = int.min(a, b)
  let mx = int.max(a, b)
  let f = int.to_float(a)
  let _ = f
}
        "#,
    );
}

#[test]
fn test_map_module_builtins() {
    assert_no_errors(
        r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let got = map.get(m, "a")
  let updated = map.set(m, "c", 3)
  let deleted = map.remove(m, "a")
  let ks = map.keys(m)
  let vs = map.values(m)
  let merged = map.merge(m, #{ "c": 3 })
  let _ = ks
}
        "#,
    );
}

#[test]
fn test_io_module_builtins() {
    assert_no_errors(
        r#"
import io
fn main() {
  let result = io.read_file("test.txt")
  let args = io.args()
  let _ = args
}
        "#,
    );
}

#[test]
fn test_option_module_builtins() {
    assert_no_errors(
        r#"
import option
fn main() {
  let opt = Some(42)
  let is_s = option.is_some(opt)
  let is_n = option.is_none(opt)
  let val = option.unwrap_or(opt, 0)
  let mapped = option.map(opt, { x -> x + 1 })
  let res = option.to_result(opt, "no value")
  let _ = val
}
        "#,
    );
}

#[test]
fn test_result_module_builtins() {
    assert_no_errors(
        r#"
import result
fn main() {
  let r = Ok(42)
  let is_ok = result.is_ok(r)
  let is_err = result.is_err(r)
  let _ = is_ok
}
        "#,
    );
}

#[test]
fn test_higher_order_builtins() {
    assert_no_errors(
        r#"
import list
fn main() {
  let xs = [[1, 2], [3, 4], [5]]
  let flat = list.flatten(xs)
  let zipped = list.zip([1, 2, 3], ["a", "b", "c"])
  let sorted = list.sort_by([3, 1, 2], { x -> x })
  let _ = flat
}
        "#,
    );
}

#[test]
fn test_len_accepts_string_and_map() {
    assert_no_errors(
        r#"
import list
import string
import map
fn main() {
  let list_len = list.length([1, 2, 3])
  let str_len = string.length("hello")
  let map_len = map.length(#{ "a": 1 })
  let _ = list_len + str_len + map_len
}
        "#,
    );
}

#[test]
fn test_assert_ne_builtin() {
    assert_no_errors(
        r#"
import test
fn main() {
  test.assert_ne(1, 2)
}
        "#,
    );
}

#[test]
fn test_channel_new_no_type_error() {
    assert_no_errors(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.close(ch)
  let _ = ch
}
        "#,
    );
}

#[test]
fn test_channel_send_mixed_types_is_error() {
    assert_has_error(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  channel.send(ch, "hello")
}
            "#,
        "type mismatch",
    );
}

#[test]
fn test_channel_receive_constrains_element_type() {
    assert_no_errors(
        r#"
import channel
fn main() {
  let ch = channel.new(10)
  channel.send(ch, 42)
  let result = channel.receive(ch)
  let _ = result
}
            "#,
    );
}

#[test]
fn test_task_spawn_no_type_error() {
    assert_no_errors(
        r#"
import task
fn main() {
  let h = task.spawn({ -> 42 })
  let result = task.join(h)
  let _ = result
}
        "#,
    );
}

#[test]
fn test_map_length_no_type_error() {
    assert_no_errors(
        r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let n = map.length(m)
  let _ = n
}
        "#,
    );
}

// ── Type narrowing after when/pattern match ────────────────────

#[test]
fn test_when_some_narrows_inner_type() {
    // After `when let Some(x) = opt`, x should have the inner type (Int)
    assert_no_errors(
        r#"
fn get_value(opt) {
  when let Some(x) = opt else {
    return 0
  }
  x + 1
}

fn main() {
  let _ = get_value(Some(42))
}
            "#,
    );
}

#[test]
fn test_when_ok_narrows_inner_type() {
    // After `when let Ok(v) = result`, v should have the ok type
    assert_no_errors(
        r#"
fn process(result) {
  when let Ok(v) = result else {
    return 0
  }
  v + 10
}

fn main() {
  let _ = process(Ok(5))
}
            "#,
    );
}

#[test]
fn test_when_some_used_in_arithmetic() {
    assert_no_errors(
        r#"
fn double_or_zero(opt) {
  when let Some(n) = opt else {
    return 0
  }
  n * 2
}

fn main() {
  let _ = double_or_zero(Some(21))
}
            "#,
    );
}

// ── Generic type inference ──────────────────────────────────────

#[test]
fn test_generic_identity_multiple_types() {
    // A generic function used with multiple types
    assert_no_errors(
        r#"
fn identity(x) {
  x
}

fn main() {
  let a = identity(42)
  let b = identity("hello")
  let c = identity(true)
  let _ = a + 1
}
            "#,
    );
}

#[test]
fn test_nested_generic_list_of_options() {
    // List<Option<Int>> — nested generic type
    assert_no_errors(
        r#"
fn main() {
  let xs = [Some(1), Some(2), None]
  let _ = xs
}
            "#,
    );
}

#[test]
fn test_generic_function_returning_generic() {
    assert_no_errors(
        r#"
fn wrap(x) {
  Some(x)
}

fn main() {
  let a = wrap(42)
  let b = wrap("hello")
  let _ = match a {
    Some(n) -> n
    None -> 0
  }
}
            "#,
    );
}

#[test]
fn test_generic_pair_function() {
    assert_no_errors(
        r#"
fn make_pair(a, b) {
  (a, b)
}

fn main() {
  let p1 = make_pair(1, "hello")
  let p2 = make_pair(true, 3.14)
  let _ = p1
}
            "#,
    );
}

// ── Recursive functions ─────────────────────────────────────────

#[test]
fn test_recursive_function() {
    assert_no_errors(
        r#"
fn factorial(n) {
  match n {
    0 -> 1
    _ -> n * factorial(n - 1)
  }
}

fn main() {
  let _ = factorial(5)
}
            "#,
    );
}

#[test]
fn test_recursive_list_function() {
    assert_no_errors(
        r#"
import list
fn sum(xs) {
  match list.first(xs) {
    None -> 0
    Some(h) -> h + sum(list.tail(xs))
  }
}

fn main() {
  let _ = sum([1, 2, 3])
}
            "#,
    );
}

// ── More exhaustiveness checking ────────────────────────────────

#[test]
fn test_match_int_without_wildcard_non_exhaustive() {
    // Matching on Int literal patterns without wildcard should be non-exhaustive
    assert_has_error(
        r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
  }
}

fn main() {
  let _ = describe(2)
}
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_string_without_wildcard_non_exhaustive() {
    // Matching on String literal patterns without wildcard should be non-exhaustive
    assert_has_error(
        r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
  }
}

fn main() {
  let _ = greet("carol")
}
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_enum_one_variant_non_exhaustive() {
    // Matching only one variant of a multi-variant enum
    assert_has_error(
        r#"
type Shape {
  Circle(Float),
  Square(Float),
  Triangle(Float, Float),
}

fn area(s) {
  match s {
    Circle(r) -> 3.14 * r * r
  }
}

fn main() {
  let _ = area(Circle(5.0))
}
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_all_guards_non_exhaustive() {
    // Guard arms don't count toward exhaustiveness
    assert_has_error(
        r#"
fn classify(n) {
  match n {
    x when x > 0 -> "positive"
    x when x < 0 -> "negative"
    x when x == 0 -> "zero"
  }
}

fn main() {
  let _ = classify(5)
}
            "#,
        "non-exhaustive",
    );
}

#[test]
fn test_match_int_with_wildcard_exhaustive() {
    // Adding a wildcard makes int matching exhaustive
    assert_no_errors(
        r#"
fn describe(n) {
  match n {
    0 -> "zero"
    1 -> "one"
    _ -> "other"
  }
}

fn main() {
  let _ = describe(2)
}
            "#,
    );
}

#[test]
fn test_match_string_with_wildcard_exhaustive() {
    assert_no_errors(
        r#"
fn greet(name) {
  match name {
    "alice" -> "hi alice"
    "bob" -> "hi bob"
    _ -> "hi stranger"
  }
}

fn main() {
  let _ = greet("carol")
}
            "#,
    );
}

// ── Error cases ─────────────────────────────────────────────────

#[test]
fn test_wrong_number_of_arguments() {
    assert_has_error(
        r#"
fn add(a, b) {
  a + b
}

fn main() {
  let _ = add(1, 2, 3)
}
            "#,
        "argument",
    );
}

#[test]
fn test_too_few_arguments() {
    assert_has_error(
        r#"
fn add(a, b) {
  a + b
}

fn main() {
  let _ = add(1)
}
            "#,
        "argument",
    );
}

#[test]
fn test_access_nonexistent_record_field() {
    assert_has_error(
        r#"
type Point { x: Int, y: Int }

fn main() {
  let p = Point { x: 1, y: 2 }
  p.z
}
            "#,
        "type 'Point' has no field 'z'",
    );
}

#[test]
fn test_undefined_variable() {
    assert_has_error(
        r#"
fn main() {
  let x = 1
  y + x
}
            "#,
        "undefined variable",
    );
}

#[test]
fn test_arithmetic_on_string_and_int() {
    // String + Int is rejected: `+` is numeric only
    assert_has_error(
        r#"
fn main() {
  "hello" + 42
}
            "#,
        "requires Int or Float",
    );
}

#[test]
fn test_boolean_and_with_non_bool() {
    assert_has_error(
        r#"
fn main() {
  let x = "hello" && true
  let _ = x
}
            "#,
        "type mismatch",
    );
}

#[test]
fn test_int_minus_string() {
    assert_has_error(
        r#"
fn main() {
  42 - "hello"
}
            "#,
        "operator '-'",
    );
}

// ── Set type inference ──────────────────────────────────────────

#[test]
fn test_set_literal_inference() {
    assert_no_errors(
        r#"
fn main() {
  let s = #[1, 2, 3]
  let _ = s
}
            "#,
    );
}

#[test]
fn test_empty_set_literal() {
    assert_no_errors(
        r#"
fn main() {
  let s = #[]
  let _ = s
}
            "#,
    );
}

#[test]
fn test_set_of_strings() {
    assert_no_errors(
        r#"
fn main() {
  let s = #["hello", "world"]
  let _ = s
}
            "#,
    );
}

// ── Loop/recur ──────────────────────────────────────────────────

#[test]
fn test_loop_basic() {
    assert_no_errors(
        r#"
fn main() {
  let _ = loop n = 0 {
    match n > 10 {
      true -> n
      false -> loop(n + 1)
    }
  }
}
            "#,
    );
}

#[test]
fn test_loop_with_accumulator() {
    assert_no_errors(
        r#"
fn main() {
  let _ = loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1, acc + i)
    }
  }
}
            "#,
    );
}

#[test]
fn test_loop_recur_arity_mismatch() {
    // loop has 2 bindings, recur has 1 argument.
    //
    // The message produced by typechecker/inference.rs is
    // `loop has N binding(s), but `loop(...)` supplies M argument(s)`.
    // The lock requires both specific phrases, "loop has" and
    // "`loop(...)` supplies" (a substring like "binding" or "argument"
    // would match unrelated diagnostics), and Severity::Error: a recur
    // arity mismatch is an Error, not a Warning.
    let errors = check_errors(
        r#"
fn main() {
  let _ = loop i = 0, acc = 0 {
    match i >= 10 {
      true -> acc
      false -> loop(i + 1)
    }
  }
}
            "#,
    );
    let recur_err = errors
        .iter()
        .find(|e| e.message.contains("loop has") && e.message.contains("`loop(...)` supplies"))
        .unwrap_or_else(|| {
            panic!(
                "expected a recur arity diagnostic containing both \"loop has\" and \
                     \"`loop(...)` supplies\", got: {:?}",
                errors.iter().map(|e| &e.message).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        recur_err.severity,
        Severity::Error,
        "recur arity mismatch must be Error severity, got {:?} for message {:?}",
        recur_err.severity,
        recur_err.message
    );
}

// ── Trait system edge cases ─────────────────────────────────────

#[test]
fn test_trait_impl_with_wrong_method_signature() {
    // Both trait methods are declared abstract (no body) so the impl
    // genuinely owes both. A method with a default body is the impl's
    // without being written, and is not reported as missing.
    let errors = check_errors(
        r#"
trait Describable {
  fn describe(self) -> String
  fn summary(self) -> String
}

type Widget { label: String }

trait Describable for Widget {
  fn describe(self) -> String { "widget" }
}

fn main() { let _ = 0 }
            "#,
    );
    assert!(
        errors.iter().any(|e| e.message.contains("missing method")),
        "expected missing method error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_trait_unknown_in_impl() {
    assert_has_error(
        r#"
type Foo { x: Int }

trait DoesNotExist for Foo {
  fn bar(self) -> Int { 0 }
}

fn main() { let _ = 0 }
            "#,
        "not declared",
    );
}

#[test]
fn test_where_clause_unknown_trait() {
    let errors = check_errors(
        r#"
fn do_thing(x) where x: FakeTrait {
  x
}

fn main() { let _ = 0 }
            "#,
    );
    assert!(
        errors.iter().any(|e| e.message.contains("FakeTrait")),
        "expected unknown trait error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_multiple_trait_impls_for_same_type() {
    // Implementing two different traits for the same type should be fine
    let errors = check_errors(
        r#"
trait Printable {
  fn print(self) -> String { "default" }
}

trait Serializable {
  fn serialize(self) -> String { "default" }
}

type Item { name: String }

trait Printable for Item {
  fn print(self) -> String { "item" }
}

trait Serializable for Item {
  fn serialize(self) -> String { "serialized" }
}

fn main() { let _ = 0 }
            "#,
    );
    // Should not produce "not declared" or "missing method" errors
    let bad_errors: Vec<_> = errors
        .iter()
        .filter(|e| e.message.contains("not declared") || e.message.contains("missing method"))
        .collect();
    assert!(
        bad_errors.is_empty(),
        "unexpected trait errors: {:?}",
        bad_errors
    );
}

// ── Ascription (as) ─────────────────────────────────────────────

#[test]
fn test_valid_ascription() {
    assert_no_errors(
        r#"
fn main() {
  let x = 42 as Int
  let _ = x
}
            "#,
    );
}

#[test]
fn test_ascription_string() {
    assert_no_errors(
        r#"
fn main() {
  let s = "hello" as String
  let _ = s
}
            "#,
    );
}

#[test]
fn test_ascription_incompatible_type() {
    assert_has_error(
        r#"
fn main() {
  let x = 42 as String
  let _ = x
}
            "#,
        "type mismatch",
    );
}

// ── Import-dependent type checking ──────────────────────────────

#[test]
fn test_string_module_split() {
    assert_no_errors(
        r#"
import string
fn main() {
  let parts = string.split("a,b,c", ",")
  let _ = parts
}
            "#,
    );
}

#[test]
fn test_list_map_and_filter() {
    assert_no_errors(
        r#"
import list
fn main() {
  let xs = [1, 2, 3, 4, 5]
  let doubled = list.map(xs, { x -> x * 2 })
  let evens = list.filter(xs, { x -> x > 2 })
  let _ = doubled
}
            "#,
    );
}

#[test]
fn test_map_get_returns_option() {
    assert_no_errors(
        r#"
import map
fn main() {
  let m = #{ "a": 1, "b": 2 }
  let result = map.get(m, "a")
  let _ = match result {
    Some(v) -> v
    None -> 0
  }
}
            "#,
    );
}

#[test]
fn test_chained_module_calls() {
    assert_no_errors(
        r#"
import string
import list
fn main() {
  let s = "Hello World"
  let result = s
    |> string.to_lower
    |> string.split(" ")
    |> list.length
  let _ = result
}
            "#,
    );
}

// ── Additional edge cases ───────────────────────────────────────

#[test]
fn test_nested_match_exhaustive() {
    // Nested Result<Option<Int>> fully covered
    assert_no_errors(
        r#"
fn process(r) {
  match r {
    Ok(Some(x)) -> x
    Ok(None) -> -1
    Err(_) -> -2
  }
}

fn main() {
  let _ = process(Ok(Some(42)))
}
            "#,
    );
}

#[test]
fn test_enum_match_all_variants_exhaustive() {
    assert_no_errors(
        r#"
type Direction {
  North,
  South,
  East,
  West,
}

fn to_string(d) {
  match d {
    North -> "north"
    South -> "south"
    East -> "east"
    West -> "west"
  }
}

fn main() {
  let _ = to_string(North)
}
            "#,
    );
}

#[test]
fn test_record_update_type_checks() {
    assert_no_errors(
        r#"
type Config {
  host: String,
  port: Int,
}

fn main() {
  let c = Config { host: "localhost", port: 8080 }
  let c2 = c.{ port: 9090 }
  let _ = c2.host
}
            "#,
    );
}

#[test]
fn test_question_mark_on_non_result() {
    // Using ? on a non-Result/Option type should error
    assert_has_error(
        r#"
fn main() -> Result {
  let x = 42?
  x
}
            "#,
        "requires Result or Option",
    );
}

// ── Unification unit tests ─────────────────────────────────────

#[test]
fn test_unify_occurs_check() {
    // Unifying Var(0) with List(Var(0)) should produce an infinite type error
    let mut tc = TypeChecker::new();
    let var = tc.fresh_var(); // Type::Var(0)
    let list_of_var = Type::List(Box::new(var.clone()));
    tc.unify(&var, &list_of_var, Span::BUILTIN);
    assert!(
        !tc.errors.is_empty(),
        "occurs check should produce an error"
    );
    assert!(
        tc.errors[0].message.contains("infinite type"),
        "expected 'infinite type' error, got: {}",
        tc.errors[0].message
    );
}

#[test]
fn test_unify_function_arity_mismatch() {
    // Unifying Function([Int], Int) with Function([Int, Int], Int) should error
    let mut tc = TypeChecker::new();
    let fn1 = Type::Fun(vec![Type::Int], Box::new(Type::Int));
    let fn2 = Type::Fun(vec![Type::Int, Type::Int], Box::new(Type::Int));
    tc.unify(&fn1, &fn2, Span::BUILTIN);
    assert!(
        !tc.errors.is_empty(),
        "function arity mismatch should produce an error"
    );
    assert!(
        tc.errors[0].message.contains("expects") && tc.errors[0].message.contains("argument"),
        "expected arity diagnostic, got: {}",
        tc.errors[0].message
    );
}

#[test]
fn test_unify_basic_var_with_int() {
    // Unifying Var(0) with Int should map Var(0) -> Int
    let mut tc = TypeChecker::new();
    let var = tc.fresh_var(); // Type::Var(0)
    tc.unify(&var, &Type::Int, Span::BUILTIN);
    assert!(tc.errors.is_empty(), "basic unification should not error");
    let resolved = tc.apply(&var);
    assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
}

#[test]
fn test_unify_transitive() {
    // Unify Var(0) with Var(1), then Var(1) with String.
    // Resolving Var(0) should yield String.
    let mut tc = TypeChecker::new();
    let var0 = tc.fresh_var(); // Type::Var(0)
    let var1 = tc.fresh_var(); // Type::Var(1)
    tc.unify(&var0, &var1, Span::BUILTIN);
    tc.unify(&var1, &Type::String, Span::BUILTIN);
    assert!(
        tc.errors.is_empty(),
        "transitive unification should not error"
    );
    let resolved = tc.apply(&var0);
    assert_eq!(
        resolved,
        Type::String,
        "Var(0) should transitively resolve to String"
    );
}

#[test]
fn test_unify_list() {
    // Unifying List(Var(0)) with List(Int) should resolve Var(0) to Int
    let mut tc = TypeChecker::new();
    let var = tc.fresh_var(); // Type::Var(0)
    let list_var = Type::List(Box::new(var.clone()));
    let list_int = Type::List(Box::new(Type::Int));
    tc.unify(&list_var, &list_int, Span::BUILTIN);
    assert!(tc.errors.is_empty(), "list unification should not error");
    let resolved = tc.apply(&var);
    assert_eq!(resolved, Type::Int, "Var(0) should resolve to Int");
}

#[test]
fn an_arm_that_is_no_result_after_a_result_arm_gets_the_ok_wrap_fix() {
    // The arm reported is the one that differs from the arms before it:
    // what is expected is their type, so an arm that is no `Result`
    // after an `Err(..)` arm is offered `Ok(...)`.
    let errors = check_errors(
        "fn checked(n: Int) -> Result(Int, String) {\n  match n {\n    0 -> Err(\"zero\")\n    _ -> n\n  }\n}\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].message,
        "type mismatch: expected Result(_, String), got Int"
    );
    assert!(errors[0].help.is_empty(), "{:?}", errors[0].help);
    let titles: Vec<&str> = errors[0].fixes.iter().map(|f| f.title.as_str()).collect();
    assert_eq!(titles, ["Wrap expression in `Ok(...)`"]);
}

#[test]
fn a_pattern_of_another_type_than_a_result_gets_no_fix_and_no_help() {
    // The `Ok(...)` fix and the chain help are about an expression: a
    // pattern's mismatch has neither.
    let errors = check_errors(
        "fn f(r: Result(Int, String)) -> Int {\n  match r {\n    5 -> 1\n    _ -> 0\n  }\n}\n",
    );
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].message,
        "type mismatch: expected Result(Int, String), got Int"
    );
    assert!(errors[0].fixes.is_empty(), "{:?}", errors[0].fixes);
    assert!(errors[0].help.is_empty(), "{:?}", errors[0].help);
}

// ── A decoder's type argument ──────────────────────────────────

/// The checker refuses a type a decoder has no decoder for, naming the
/// decoder, the field and its type, at the type argument, next to the
/// other type errors.
#[test]
fn a_decoder_s_type_argument_with_an_undecodable_field_is_refused() {
    let source = r#"
import json
type Bag { items: Set(Int) }
fn main() {
  let n: Int = "x"
  let _ = json.parse("x", Bag)
}
"#;
    let errors = check_errors(source);
    let refusal = errors
        .iter()
        .find(|e| e.code == Code::Undecodable)
        .expect("the refusal");
    assert!(
        refusal.message.contains("json.parse")
            && refusal.message.contains("`items`")
            && refusal.message.contains("Set(Int)"),
        "expected the error to name the call, the field and its type, got: {}",
        refusal.message
    );
    let at = refusal.span.start as usize;
    assert_eq!(&source[at..at + 3], "Bag", "at the type argument");
    assert!(
        errors.iter().any(|e| e.code == Code::TypeMismatch),
        "with the type error: {errors:?}"
    );
}

/// The fields of a record at a wrong number of type arguments are at
/// fresh variables, never at the record's own parameter variables,
/// which every use of the record shares; at the right number, at the
/// arguments.
#[test]
fn record_fields_at_a_wrong_number_of_arguments_take_fresh_variables() {
    use crate::intern::intern;
    let mut tc = TypeChecker::new();
    let rec = TypeRef {
        id: crate::defs::TypeId(crate::defs::DefId(u32::MAX - 2)),
        name: intern("FieldsAtArgsBox"),
    };
    let Type::Var(param) = tc.fresh_var() else {
        unreachable!("a fresh variable")
    };
    tc.tables.record_param_var_ids.insert(rec, vec![param]);
    tc.tables.records.insert(
        rec,
        RecordInfo {
            fields: vec![(intern("v"), Type::Var(param))],
            params: vec![intern("a")],
        },
    );
    let at_int = tc.instantiate_record_fields_with_args(rec, &[Type::Int]);
    assert_eq!(at_int, vec![(intern("v"), Type::Int)]);
    for args in [&[][..], &[Type::Int, Type::Bool][..]] {
        let fields = tc.instantiate_record_fields_with_args(rec, args);
        match fields.as_slice() {
            [(v, Type::Var(var))] if *v == intern("v") => {
                assert_ne!(*var, param, "the template variable leaked at {args:?}")
            }
            other => panic!("expected one field at a fresh variable, got {other:?}"),
        }
    }
}

// ── The one check of a decoder's undecided type ─────────────────────

/// Each decoder given a generic type or alias written alone whose type
/// arguments nothing decides is judged by the one check, also where an
/// earlier refusal with a text of its own speaks for the use; a type
/// written in full, and one a parameter holding a type gives, is not.
#[test]
fn the_one_check_judges_every_decoder_given_an_undecided_type() {
    super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow_mut().clear());
    let errors = check_errors(
        r#"
import json
import toml
type Box(a) { v: a }
type L(a) = List(a)
fn held(s, t) { json.parse(s, List(t)) }
fn main() {
  let a = json.parse("[1]", L)
  let b = json.decode(json.Null, Option(L))
  let c = toml.parse_map("a = 1", L)
  let e = json.parse("[0]", List(Box))
  let f = json.parse("[[0]]", (L, Int))
  let g = json.parse("[1]", List(Int))
  let h = held("[1]", Int)
  println("{a} {b} {c} {e} {f} {g} {h}")
}
        "#,
    );
    assert!(errors.iter().any(|e| e.is_error()));
    let judged = super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow().clone());
    assert_eq!(
        judged,
        [
            "json.parse",
            "json.decode",
            "toml.parse_map",
            "json.parse",
            "json.parse"
        ],
        "the decoders judged undecided"
    );
}

/// The one check judges a decode owed through a function that hands
/// its parameter to a decoder (`load(s, L)`), through a method of a
/// trait with `json.Decode` among its supertraits called on a value
/// whose part a scope's end fixed to `()` (`twice([], s)`, `Dec.again([],
/// s)`), and through `postgres.query`'s type argument.
#[test]
fn the_one_check_judges_decodes_through_functions_and_traits() {
    super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow_mut().clear());
    let errors = check_errors(
        r#"
import json
trait Dec: json.Decode {
  fn again(self, s: String) -> Result(Self, json.JsonError) { json.parse(s, Self) }
}
trait Dec for List(a) where a: json.Decode {}
type L(a) = List(a)
fn load(s, t) { json.parse(s, t) }
fn twice(b, s) { Dec.again(b, s) }
fn main() {
  println(load("[1]", L))
  println(twice([], "[1]"))
  println(Dec.again([], "[1]"))
  println(load("[1]", List(Int)))
  println(twice([1], "[1]"))
}
        "#,
    );
    assert!(errors.iter().any(|e| e.is_error()));
    let judged = super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow().clone());
    assert_eq!(
        judged,
        ["load", "twice", "again"],
        "the uses judged undecided"
    );
}

#[cfg(feature = "postgres")]
#[test]
fn the_one_check_judges_a_rows_type_given_to_postgres() {
    super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow_mut().clear());
    let errors = check_errors(
        r#"
import postgres
type Row(a) { id: a }
fn main() {
  match postgres.connect("postgres://localhost/x") {
    Ok(db) -> println(postgres.query(db, "SELECT 1", (), Row))
    Err(e) -> println(e)
  }
}
        "#,
    );
    assert!(errors.iter().any(|e| e.is_error()));
    let judged = super::json_decode::JUDGED_UNDECIDED.with(|judged| judged.borrow().clone());
    assert_eq!(judged, ["postgres.query"], "the uses judged undecided");
}

// ── The numbers of a type's variants, fields and parameters ─────

/// `type Big { V0, V1, … }` with `variants` variants, and a `main` that
/// tells the last one from the first.
fn enum_of(variants: usize) -> String {
    let names: Vec<String> = (0..variants).map(|k| format!("V{k}")).collect();
    let last = variants - 1;
    format!(
        "type Big {{ {} }}\n\
         fn name(b: Big) -> String {{\n  match b {{\n    V0 -> \"first\"\n    V{last} -> \"last\"\n    _ -> \"other\"\n  }}\n}}\n\
         fn main() {{ [name(V0), name(V{last})] }}\n",
        names.join(", ")
    )
}

/// The ordinal of a variant is a `u16`: an enum of 65,537 variants
/// would number its last one 0, its first. It is refused at its name
/// with the wording of every compile limit.
#[test]
fn test_more_than_65536_variants_refused() {
    let source = enum_of(65_537);
    let errors = check_errors(&source);
    let limit: Vec<_> = errors
        .iter()
        .filter(|e| e.code == Code::CompileLimit)
        .collect();
    assert_eq!(
        limit.len(),
        1,
        "{:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert_eq!(
        limit[0].message,
        "too many variants of a type: 65537 (the limit is 65536)"
    );
    let span = limit[0].span;
    assert_eq!(&source[span.start as usize..span.end as usize], "Big");
}

// ── A decoder's type argument that is no type ───────────────────

/// An undefined name in the type a decoder is given is the error; the
/// type it left undecided is not asked about as well, through a
/// function that passes the type on neither.
#[test]
fn test_undefined_name_in_a_decoder_type_is_the_one_error() {
    let errors = check_errors(
        "import json\n\
         type Box(a) { v: a }\n\
         fn load(text: String, type a) -> Result(a, json.JsonError) where a: json.Decode {\n\
         \x20 json.parse(text, a)\n\
         }\n\
         fn main() {\n\
         \x20 println(json.parse(\"null\", Box(Box(Nope))))\n\
         \x20 println(load(\"null\", Box(Box(Nope))))\n\
         }\n",
    );
    let messages: Vec<&str> = (errors.iter())
        .filter(|e| e.is_error())
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        messages,
        ["undefined variable 'Nope'", "undefined variable 'Nope'"]
    );
}

/// A type given a value (`List(())`) says the argument is a value, at
/// the argument, not that the type is not callable.
#[test]
fn test_a_type_given_a_value_says_the_argument_is_a_value() {
    let source = "import json\nfn main() {\n  println(json.parse(\"null\", List(())))\n}\n";
    let errors = check_errors(source);
    let errors: Vec<_> = errors.iter().filter(|e| e.is_error()).collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(
        errors[0].message,
        "`List` takes a type here, and `()` is a value"
    );
    let span = errors[0].span;
    assert_eq!(&source[span.start as usize..span.end as usize], "()");
}

/// Any generic type given a value says so, by the value's text when it
/// is a short literal, else by its type.
#[test]
fn test_a_generic_type_given_a_value_names_the_value() {
    let errors = check_errors(
        "import json\ntype Box(a) { v: a }\nfn main() {\n  let n = 1\n  println(json.parse(\"null\", Box(1)))\n  println(json.parse(\"null\", Box(n)))\n}\n",
    );
    let messages: Vec<&str> = (errors.iter())
        .filter(|e| e.is_error())
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "`Box` takes a type here, and `1` is a value",
            "`Box` takes a type here, and a value of type `Int` is given",
        ]
    );
}
