use super::super::test_helpers::*;

// ── Unary operator inference ────────────────────────────────────

#[test]
fn test_unary_negate_int() {
    assert_no_errors(
        r#"
fn main() {
  let x = -42
  x
}
        "#,
    );
}

#[test]
fn test_unary_negate_float() {
    assert_no_errors(
        r#"
fn main() {
  let x = -3.14
  x
}
        "#,
    );
}

#[test]
fn test_unary_not_bool() {
    assert_no_errors(
        r#"
fn main() {
  let x = !true
  x
}
        "#,
    );
}

#[test]
fn test_unary_not_non_bool() {
    assert_has_error(
        r#"
fn main() {
  !42
}
        "#,
        "type mismatch",
    );
}

// ── Or-pattern binding ──────────────────────────────────────────

#[test]
fn test_or_pattern_binds_variable() {
    assert_no_errors(
        r#"
fn classify(x) {
  match x {
    1 | 2 | 3 -> "small"
    _ -> "big"
  }
}
fn main() { classify(2) }
        "#,
    );
}

#[test]
fn test_or_pattern_with_constructor_binding() {
    assert_no_errors(
        r#"
fn extract(x) {
  match x {
    Ok(v) | Err(v) -> v
  }
}
fn main() { extract(Ok(42)) }
        "#,
    );
}

// ── Map pattern binding ─────────────────────────────────────────

#[test]
fn test_map_pattern_in_match() {
    assert_no_errors(
        r#"
fn main() {
  let m = #{ "x": 1, "y": 2 }
  match m {
    #{ "x": val } -> val
    _ -> 0
  }
}
        "#,
    );
}

// ── Pin pattern ─────────────────────────────────────────────────

#[test]
fn test_pin_pattern_matches_value() {
    assert_no_errors(
        r#"
fn main() {
  let expected = 42
  match 42 {
    ^expected -> "matched"
    _ -> "no match"
  }
}
        "#,
    );
}

// ── Return expression ───────────────────────────────────────────

#[test]
fn test_return_with_value() {
    assert_no_errors(
        r#"
fn early(x) {
  when x > 0 else { return 0 }
  x * 2
}
fn main() { early(5) }
        "#,
    );
}

#[test]
fn test_return_no_value() {
    assert_no_errors(
        r#"
fn side_effect(x) {
  when x > 0 else { return () }
  println(x)
}
fn main() { side_effect(1) }
        "#,
    );
}

// ── String interpolation inference ──────────────────────────────

#[test]
fn test_string_interp_with_int_and_bool() {
    assert_no_errors(
        r#"
fn main() {
  let n = 42
  let b = true
  "n={n}, b={b}"
}
        "#,
    );
}

// ── Ascription ──────────────────────────────────────────────────

#[test]
fn test_ascription_correct_type() {
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
fn test_ascription_mismatch() {
    assert_has_error(
        r#"
fn main() {
  "hello" as Int
}
        "#,
        "type mismatch",
    );
}

// ── Pipe operator inference ─────────────────────────────────────

#[test]
fn test_pipe_chains_types() {
    assert_no_errors(
        r#"
fn double(x) { x * 2 }
fn add_one(x) { x + 1 }
fn main() {
  5 |> double |> add_one
}
        "#,
    );
}

// ── Record update inference ─────────────────────────────────────

#[test]
fn test_record_update_preserves_type() {
    assert_no_errors(
        r#"
type Point { x: Int, y: Int }
fn main() {
  let p = Point { x: 1, y: 2 }
  let q = p.{ x: 10 }
  q.y
}
        "#,
    );
}

#[test]
fn test_record_create_wrong_field_type() {
    assert_has_error(
        r#"
type Point { x: Int, y: Int }
fn main() {
  Point { x: "hello", y: 2 }
}
        "#,
        "type mismatch",
    );
}

// ── check_pattern error cases ───────────────────────────────────

#[test]
fn test_check_pattern_wrong_type() {
    assert_has_error(
        r#"
fn main() {
  match 42 {
    true -> "yes"
    false -> "no"
  }
}
        "#,
        "type mismatch",
    );
}

#[test]
fn test_list_rest_pattern_binds() {
    assert_no_errors(
        r#"
fn sum_list(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum_list(tail)
  }
}
fn main() { sum_list([1, 2, 3]) }
        "#,
    );
}

// ── Range pattern type checking ─────────────────────────────────

#[test]
fn test_range_pattern_int() {
    assert_no_errors(
        r#"
fn classify(n) {
  match n {
    1..10 -> "small"
    _ -> "big"
  }
}
fn main() { classify(5) }
        "#,
    );
}

// ── When-bool statement ─────────────────────────────────────────

#[test]
fn test_when_bool_condition_must_be_bool() {
    assert_has_error(
        r#"
fn check(x) {
  when 42 else { return 0 }
  x
}
fn main() { check(1) }
        "#,
        "type mismatch",
    );
}

// ── Loop/recur type inference ───────────────────────────────────

#[test]
fn test_loop_bindings_inferred() {
    assert_no_errors(
        r#"
fn factorial(n) {
  loop i = n, acc = 1 {
    match i <= 1 {
      true -> acc
      false -> loop(i - 1, acc * i)
    }
  }
}
fn main() { factorial(5) }
        "#,
    );
}

// ── Trait constraint checking at definition ────────────────────

#[test]
fn test_trait_constraint_method_resolved() {
    // A constrained type variable should allow calling trait methods
    assert_no_errors(
        r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.display()
}
fn main() { show(42) }
        "#,
    );
}

#[test]
fn test_trait_constraint_unknown_method_errors() {
    // A constrained type variable should NOT allow calling methods not in the trait
    assert_has_error(
        r#"
trait Display for a {
  fn display(self) -> String { "?" }
}
fn show(x: a) -> String where a: Display {
  x.nonexistent()
}
fn main() { show(42) }
        "#,
        "no method 'nonexistent' found in trait constraints",
    );
}

// ── Error type propagation ─────────────────────────────────────

#[test]
fn test_error_type_does_not_produce_fresh_var() {
    // Accessing a field on an error type should propagate the error,
    // not create a new unresolved type variable
    assert_has_error(
        r#"
fn main() {
  let x = undefined_var
  x.field
}
        "#,
        "undefined",
    );
}

// ── B2: arithmetic on String should be rejected ────────────────

#[test]
fn test_string_add_is_rejected_naming_interpolation() {
    assert_has_error(
        r#"
fn main() {
  "hello" + " world"
}
        "#,
        "build strings with interpolation",
    );
}

#[test]
fn test_string_sub_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  "hello" - "world"
}
        "#,
        "requires Int or Float",
    );
}

#[test]
fn test_string_mul_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  "hello" * "world"
}
        "#,
        "requires Int or Float",
    );
}

#[test]
fn test_string_mod_is_rejected() {
    assert_has_error(
        r#"
fn main() {
  "hello" % "world"
}
        "#,
        "requires Int or Float",
    );
}

// ── L1: Parameterized records should get fresh type vars ───────

#[test]
fn test_parameterized_record_different_instantiations() {
    assert_no_errors(
        r#"
type Box(a) { value: a }
fn main() {
  let int_box = Box { value: 42 }
  let str_box = Box { value: "hello" }
  int_box.value + 1
  str_box.value == "hello"
}
        "#,
    );
}

// ── Constructor arity in let bindings ──────────────────────────

#[test]
fn test_let_constructor_wrong_arity_is_type_error() {
    assert_has_error(
        r#"
type Maybe(T) {
  None,
  Some(T),
}
fn main() {
  let Some(x, y) = Some(42)
  0
}
        "#,
        "constructor 'Some' expects 1 field, but pattern has 2",
    );
}

#[test]
fn test_let_nested_constructor_wrong_arity_is_type_error() {
    assert_has_error(
        r#"
type Maybe(T) {
  None,
  Some(T),
}
type Pair(A, B) {
  P(A, B),
}
fn main() {
  let P(Some(x, y), b) = P(Some(42), 1)
  0
}
        "#,
        "constructor 'Some' expects 1 field, but pattern has 2",
    );
}
