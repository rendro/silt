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

## Supertrait Bounds

A trait can declare other traits as **supertraits** using `: Trait` after
the trait name. Implementing the subtrait then requires the type to also
implement every supertrait, and methods from the supertrait become
callable through the subtrait constraint:

```silt
trait Equal {
  fn equal(self, other: Self) -> Bool
}

trait Ordered: Equal {
  fn less(self, other: Self) -> Bool
}
```

Implementing `Ordered` on `Int` requires `Int` to also implement `Equal`
(the four built-in traits — `Equal`, `Hash`, `Compare`, `Display` — are
auto-derived for every type, so the obligation is satisfied automatically).

Multiple supertraits separate with `+`:

```silt
trait Printable: Display + Hash {
  fn print_with_hash(self) -> String
}
```

### Constraint expansion

Inside a `where a: Ordered` body, methods from `Equal` (the supertrait)
are also callable on `a`:

```silt
fn check(a: t, b: t) -> Bool where t: Ordered {
  -- a.equal(b) works because Equal is a supertrait of Ordered
  match a.equal(b) {
    true -> true
    false -> a.less(b)
  }
}
```

The expansion is transitive: with `trait C: B { ... }` and
`trait B: A { ... }`, a `where x: C` constraint enables methods from
`A`, `B`, and `C` on `x`, and implementing `C` on a type requires impls
of `A`, `B`, and `C`.

### Errors

Unknown supertrait names are rejected at the trait declaration:

```silt
trait Foo: NotATrait { ... }
-- error: trait 'Foo' lists unknown supertrait 'NotATrait'
```

Implementing a subtrait without the supertrait fails:

```silt
type MyInt { v: Int }
trait Ordered for MyInt { ... }
-- error: type 'MyInt' implements 'Ordered' but does not implement supertrait 'Equal'
-- (only fires when MyInt does not have an Equal impl — auto-derived counts)
```

## Default Methods

A trait method can carry a body inside the trait declaration itself. The
body is the **default** implementation: any impl that omits the method
inherits it as if the impl had pasted the body in directly. Impls remain
free to override the default.

```silt
trait Display {
  fn show(self) -> String { "default" }   -- default body
  fn debug(self) -> String                 -- abstract method (no body)
}

type Item { v: Int }

trait Display for Item {
  fn debug(self) -> String { "item-debug" }
  -- show() is omitted — the default "default" is used at runtime
}

Item { v: 1 }.show()    -- "default"
Item { v: 1 }.debug()   -- "item-debug"
```

A trait can mix default and abstract methods freely:

- Methods with `{ ... }` or `= ...` bodies are **defaults**. Impls may
  omit them; if they do, the default body is used.
- Methods without a body are **abstract**. Impls must provide them.

Overriding a default is just writing the method in the impl as usual:

```silt
trait Display for Item {
  fn show(self) -> String { "explicit-show" }   -- overrides the default
  fn debug(self) -> String { "item-debug" }
}
```

Default bodies can call other trait methods on `self`, including
abstract ones the impl is required to provide. Dispatch routes the call
to the impl's version, so the default acts as a template that
specialises per impl:

```silt
trait Describable {
  fn name(self) -> String                              -- abstract
  fn greet(self) -> String { "hi, " + self.name() }    -- default uses name()
}

type Person { who: String }

trait Describable for Person {
  fn name(self) -> String { self.who }
}

Person { who: "alice" }.greet()    -- "hi, alice"
```

Defaults compose with supertraits: a default body may call a supertrait
method on `self`, and the obligation that the supertrait be implemented
is enforced as usual.

Defaults work on parameterized impl targets too — `trait X for Box(a) { }`
inherits every default the trait declares.

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
