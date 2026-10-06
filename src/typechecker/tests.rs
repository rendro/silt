use super::test_helpers::*;
use super::*;

// ── Basic type inference ────────────────────────────────────────

#[test]
fn test_int_literal() {
    assert_no_errors(
        r#"
fn main() {
  let x = 42
  x
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
  x
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
  x
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
  x
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
  y
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
  x
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
  add(1, 2)
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
  x
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
  xs
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
  xs
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
  pair
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
  double(5)
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
  area(Circle(5.0))
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
  u.name
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
  u2.age
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
  describe(Circle(1.0))
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
  describe(Circle(1.0))
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
  name(Red)
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
fn main() { handle(Ok(Some(1))) }
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
fn main() { handle(Ok(Some(1))) }
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
fn main() { check((true, true)) }
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
fn main() { check((true, true)) }
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
  match x {
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
  match x {
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
  match process(21) {
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
  match process(21) {
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
  check(5)
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
  match process(21) {
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
  [1, 2, 3, 4, 5]
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
  "hello {name}, the answer is {n}"
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
  s.display()
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
  m
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
  results
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
  add5(10)
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
  [classify(-5), classify(0), classify(42)]
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
  a
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
  x
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
  r
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
  classify(5)
}
        "#,
    );
}

// ── Severity tests ─────────────────────────────────────────────

#[test]
fn test_type_error_has_error_severity() {
    // A type mismatch should produce Severity::Error.
    //
    // LATENT fix (audit round 36): previously this test only asserted that
    // *some* error with Error severity existed — any unrelated diagnostic
    // with Error severity would satisfy it. Narrow the lock to the specific
    // Int/String mismatch under test: find the diagnostic whose message
    // mentions both "Int" and "String" and assert IT has Error severity.
    // Per the "test must fail on a mutated source" rule for weak-lock
    // strengthenings: if the typechecker regressed to produce the Int/String
    // mismatch as a Warning, this strengthened assertion would fail where
    // the old `any()` check would still pass due to unrelated errors.
    let errors = check_errors(
        r#"
            fn main() {
                let x: Int = "hello"
                x
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
                x + 1
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
            fn main() { 0 }
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
    // default. With the default-method feature, a method with a body
    // would be synthesized into the impl rather than reported.
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
            fn main() { 0 }
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
            fn main() { 0 }
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
            fn main() { 0 }
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
            fn main() { 0 }
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
                display(Red)
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
    // GAP (round 17 F4): the previous test only bound `where x: Showable`
    // on `x` which never referenced a valid type variable — the
    // constraint-introduction check fired with a suggestion string
    // that happened to contain "Showable", and the test's disjunctive
    // assertion (`contains("does not implement") || contains("Showable")`)
    // matched the wrong branch. It was green against a codebase that
    // completely dropped the "does not implement" check.
    //
    // Pin the real path: declare `display` with a proper typed
    // parameter `x: a where a: Showable`, implement Showable for Int
    // only, then call `display("text")`. Int satisfies; String does
    // not. Must now produce "type 'String' does not implement trait
    // 'Showable'".
    let errors = check_errors(
        r#"
            trait Showable { fn show(self) -> String }
            trait Showable for Int { fn show(self) -> String { "int" } }
            fn display(x: a) -> String where a: Showable { x.show() }
            fn main() { display("text") }
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
  items: List,
  name: String,
}

fn main() {
  let b = Bag { items: [1, 2, 3], name: "test" }
  b.name
}
        "#,
    );
}

#[test]
fn test_record_with_map_field() {
    assert_no_errors(
        r#"
type Config {
  data: Map,
}

fn main() {
  let c = Config { data: #{ "key": "value" } }
  c.data
}
        "#,
    );
}

#[test]
fn test_record_with_list_and_map_fields() {
    assert_no_errors(
        r#"
type Config {
  values: Map,
  errors: List,
}

fn main() {
  let c = Config { values: #{ "a": 1 }, errors: ["err1", "err2"] }
  c.values
}
        "#,
    );
}

#[test]
fn test_record_with_list_field_access() {
    assert_no_errors(
        r#"
type Bag {
  items: List,
}

fn main() {
  let b = Bag { items: [1, 2, 3] }
  b.items
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
  let head = list.head(xs)
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
  n
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
  n
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
  rounded
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
  f
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
  let deleted = map.delete(m, "a")
  let ks = map.keys(m)
  let vs = map.values(m)
  let merged = map.merge(m, #{ "c": 3 })
  ks
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
  args
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
  val
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
  is_ok
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
  flat
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
  list_len + str_len + map_len
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
  ch
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
  result
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
  result
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
  n
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
  get_value(Some(42))
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
  process(Ok(5))
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
  double_or_zero(Some(21))
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
  a + 1
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
  xs
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
  match a {
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
  p1
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
  factorial(5)
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
  match list.head(xs) {
    None -> 0
    Some(h) -> h + sum(list.tail(xs))
  }
}

fn main() {
  sum([1, 2, 3])
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
  describe(2)
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
  greet("carol")
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
  area(Circle(5.0))
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
  classify(5)
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
  describe(2)
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
  greet("carol")
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
  add(1, 2, 3)
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
  add(1)
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
        "unknown field or method 'z' on type Point",
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
  x
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
  s
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
  s
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
  s
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
  loop n = 0 {
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
  loop i = 0, acc = 0 {
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
    // LATENT fix (audit round 36): previously the assertion was a 2-way
    // substring OR — `contains("binding") || contains("argument")` — so
    // many unrelated diagnostics could satisfy it (e.g. any diagnostic
    // that says "unused binding" or "argument count"). The real message
    // produced by typechecker/inference.rs is
    // `loop has N binding(s), but `loop(...)` supplies M argument(s)`.
    //
    // Strengthening:
    //   - AND-chain specific phrases "loop has" && "`loop(...)` supplies"
    //   - require Severity::Error (GAP #163 established recur arity
    //     mismatch is an Error, not a Warning)
    //
    // Per the "test must fail on a mutated source" rule for weak-lock
    // strengthenings: if the message were reworded, or if the emitter
    // regressed to `self.warning(...)` instead of `self.error(...)`,
    // this strengthened check would fail where the old OR-substring
    // check could still pass. The current code passes both — this is a
    // correct-just-under-locked scenario, so the strengthening is valid.
    let errors = check_errors(
        r#"
fn main() {
  loop i = 0, acc = 0 {
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
        "recur arity mismatch must be Error severity (GAP #163), got {:?} for message {:?}",
        recur_err.severity,
        recur_err.message
    );
}

// ── Trait system edge cases ─────────────────────────────────────

#[test]
fn test_trait_impl_with_wrong_method_signature() {
    // Both trait methods are declared abstract (no body) so the impl
    // genuinely owes both. Methods with default bodies are now
    // synthesized into impls rather than reported as missing.
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

fn main() { 0 }
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

fn main() { 0 }
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

fn main() { 0 }
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

fn main() { 0 }
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
  x
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
  s
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
  x
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
  parts
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
  doubled
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
  match result {
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
  result
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
  process(Ok(Some(42)))
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
  to_string(North)
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
  c2.host
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
