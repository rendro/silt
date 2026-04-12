---
title: "Traits"
section: "Language"
order: 7
---

# Traits

Traits define shared behavior. No inheritance, no subclassing, no associated
types -- just methods.

## Declaration and Implementation

```silt
trait Display {
  fn display(self) -> String
}

trait Display for Shape {
  fn display(self) -> String {
    match self {
      Circle(r) -> "Circle(r={r})"
      Rect(w, h) -> "Rect({w}x{h})"
    }
  }
}

Circle(5.0).display()   -- "Circle(r=5)"
```

## Self Type

Use `Self` in trait method signatures to refer to the implementing type:

```silt
trait Monoid {
  fn empty() -> Self
  fn combine(a: Self, b: Self) -> Self
}

trait Monoid for Int {
  fn empty() -> Self { 0 }
  fn combine(a: Self, b: Self) -> Self { a + b }
}
```

## Parameterized Implementation Targets

An impl on a parameterized record or enum can bind the target's type
parameters directly in the impl header. Lowercase names in the target's
argument list are fresh type variables scoped to every method in the impl:

```silt
type Box(T) { Box(T) }

trait Wrap {
  fn unwrap(self) -> Int
}

trait Wrap for Box(a) {
  fn unwrap(self) -> Int {
    match self {
      Box(inner) -> 1
    }
  }
}
```

The `a` in `Box(a)` is a fresh type variable. Every method in the impl
sees the same `a`, so `fn get(self) -> a` and `fn put(self, x: a)` in the
same impl refer to the same variable. At call sites, `a` monomorphises per
use — a single `trait Wrap for Box(a)` impl handles both `Box(42)` and
`Box("hello")` without separate declarations.

Rules:

- **Only lowercase binders.** `trait X for Box(Int)` is a parse error —
  silt has no specialization.
- **Binders must be distinct.** `trait X for Pair(a, a)` is a parse error.
- **Arity must match the target.** `trait X for Box(a, b)` on the 1-param
  `Box(T)` is a type error.
- **The bare form still works.** `trait X for Box { ... }` is equivalent
  to `trait X for Box(_)` — useful when the method bodies never observe
  the element type.

### Impl-level where clauses

To call a trait method on an impl-bound type variable, declare the
constraint on the **impl header** using a `where` clause. The constraint
applies to every method in the impl and is also enforced at every call
site — passing a `Box(v)` where `v`'s type does not implement the
required trait is a compile-time error:

```silt
type Box(T) { Box(T) }

trait Greet {
  fn greet(self) -> String
}

trait Greet for Int {
  fn greet(self) -> String { "int-greet" }
}

trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self {
      Box(inner) -> inner.greet()
    }
  }
}

Box(5).greet()            -- "int-greet"
Box("hello").greet()      -- error: type 'String' does not implement trait 'Greet'
```

Multi-trait bounds use `+` (or comma-separated clauses) — identical to
fn-level `where`:

```silt
trait Greet for Box(a) where a: Greet + Loud {
  fn greet(self) -> String {
    match self { Box(inner) -> "{inner.greet()}-{inner.loud()}" }
  }
}

-- equivalent:
trait Greet for Box(a) where a: Greet, a: Loud {
  fn greet(self) -> String { ... }
}
```

### Method-level where clauses

Method-level `where` clauses on trait-impl methods also work and have
the same semantics as fn-level `where`. Use them when only **one**
method in the impl needs the constraint; put it on the impl header
when every method needs it:

```silt
trait Wrap for Box(a) {
  fn wrap(self) -> Int { 1 }
  fn greet(self) -> String where a: Greet {
    match self { Box(inner) -> inner.greet() }
  }
}
```

`Box("hello").wrap()` works (no constraint); `Box("hello").greet()`
fails at the call site against the method-level `where a: Greet`.

Field access on a type-var field in a record works the same way:

```silt
type Cell(T) { value: T }

trait Peek { fn peek(self) -> Int }
trait Peek for Int { fn peek(self) -> Int { self } }

trait Peek for Cell(a) where a: Peek {
  fn peek(self) -> Int { self.value.peek() }
}
```

## Built-in Traits

| Trait     | Purpose                          |
|-----------|----------------------------------|
| `Display` | Convert to human-readable string |
| `Equal`   | Equality comparison              |
| `Hash`    | Hash value for maps/sets         |
| `Compare` | Order comparison                 |

All four are **automatically derived** for every user-defined type. The
auto-derived `Display` formats in constructor syntax (`Circle(5)`). Write
your own `trait Display for T` to override.

## Where Clauses

Constrain generic parameters to types implementing a trait. Where clauses
**must** use explicit type annotations:

```silt
-- CORRECT: 'a' appears in the parameter annotation
fn print_all(items: List(a)) where a: Display {
  items |> list.each { item -> println(item.display()) }
}

-- ERROR: 'a' is unbound -- no annotation on x
fn f(x) where a: Display {
  println(x.display())
}
```

The form `fn f(x) where a: Display` is an error because the compiler cannot
determine which parameter `a` refers to.

Multiple trait bounds use `+`:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash {
  ...
}
```

This is equivalent to `where a: Equal, a: Hash`.
