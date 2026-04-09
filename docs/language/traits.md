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
