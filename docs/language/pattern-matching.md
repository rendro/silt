---
title: "Pattern Matching"
section: "Language"
order: 3
---

# Pattern Matching

Pattern matching is the only branching construct. No `if`, no ternary, no
`switch`. Every branch uses `match`, and the compiler checks exhaustiveness.

## Match with Scrutinee

```silt
fn describe(shape) {
  match shape {
    Circle(r) -> "circle with radius {r}"
    Rect(w, h) -> "rect {w}x{h}"
  }
}
```

## Match without Scrutinee (Boolean Dispatch)

Omit the scrutinee for boolean conditions:

```silt
fn classify(n) {
  match {
    n == 0 -> "zero"
    n > 0  -> "positive"
    _      -> "negative"
  }
}
```

## Literal Patterns

Including negative numbers:

```silt
match n {
  0 -> "zero"
  1 -> "one"
  -1 -> "negative one"
  _ -> "other"
}
```

Works for `Int`, `Float`, `String`, and `Bool`.

## Constructor and Nested Patterns

```silt
fn handle(result) {
  match result {
    Ok(Some(value)) -> use(value)
    Ok(None) -> handle_empty()
    Err(e) -> handle_error(e)
  }
}
```

## Tuple Patterns

```silt
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}
```

## Record Patterns

Use `..` to ignore remaining fields:

```silt
fn greet(user) {
  match user {
    User { name, active: true, .. } -> "hello {name}"
    User { name, .. } -> "{name} is inactive"
  }
}
```

## List Patterns

Three forms: empty `[]`, exact length `[a, b, c]`, head/tail `[h, ..t]`:

```silt
fn sum(xs) {
  match xs {
    [] -> 0
    [head, ..tail] -> head + sum(tail)
  }
}
```

Nested patterns work in list elements: `[Some(a), Some(b), ..rest]`. The
`..` syntax matches the record rest pattern (`User { name, .. }`).

## Map Patterns

```silt
match config {
  #{ "name": name, "greeting": greeting } -> "{greeting}, {name}!"
  #{ "name": name } -> "hello, {name}!"
  _ -> "hello, stranger!"
}
```

## Or-Patterns

Match multiple alternatives in a single arm with `|`:

```silt
match n {
  0 | 1 -> "small"
  2 | 3 -> "medium"
  _ -> "large"
}
```

All alternatives must bind the **same** variables:

```silt
-- OK: both sides bind x
Some(x) | Ok(x) -> use(x)

-- ERROR: left binds x, right binds y
Some(x) | Ok(y) -> ...   -- compile error
```

## Guards

```silt
match n {
  0 -> "zero"
  x when x > 0 -> "positive"
  _ -> "negative"
}
```

Guard expressions are checked after the pattern matches. Pattern bindings
are available in the guard.

## Range Patterns

Inclusive numeric ranges:

```silt
match score {
  90..100 -> "A"
  80..89  -> "B"
  70..79  -> "C"
  _       -> "F"
}
```

Float ranges work too: `0.0..1.0` matches `value >= 0.0 && value <= 1.0`.

## Pin Operator (`^`)

Matches against the value of an existing variable instead of creating a new
binding:

```silt
let expected = 42
match input {
  ^expected -> "got the expected value"
  other -> "got {other} instead"
}
```

Works in any pattern position. Common with `channel.select`:

```silt
match channel.select([ch1, ch2]) {
  (^ch1, Message(msg)) -> handle_first(msg)
  (^ch2, Message(msg)) -> handle_second(msg)
  _ -> panic("unexpected")
}
```

## `when let`/`else` for Inline Assertions

Asserts a pattern match and binds on success, or diverges on failure:

```silt
fn process(input) {
  when let Ok(value) = parse(input) else { return Err("parse failed") }
  when let Admin(perms) = user.role else { return Err("unauthorized") }
  do_admin_thing(value, perms)
}
```

The `else` block **must** diverge (`return` or `panic`). A boolean form
(`when cond else { ... }`) also exists for flat guard sequences, and both
forms can be mixed.

See [Error Handling](error-handling.md#when-let-else-for-custom-errors) for
the full treatment, including when to reach for `when let`-`else` instead
of `?`.

## Exhaustiveness Checking

The compiler checks that your match covers all possible cases. Missing a
variant produces a compile-time error. This is one of the strongest benefits
of `match` as the sole branching construct.

**Trade-off: no `if`.** Simple boolean checks are more verbose (`match debug
{ true -> ..., false -> () }`). In practice, guardless match and `when`-`else`
cover most cases.
