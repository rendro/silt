---
title: "Traits"
section: "Language"
order: 7
---

# Traits

Traits define shared behavior. No inheritance, no subclassing -- methods,
plus associated types (a trait may declare a `type Item` member that each
impl binds; see [Generics — Associated types](generics.md#associated-types)).

## Declaration and Implementation

```silt
type Shape {
  Circle(Float),
  Rect(Float, Float),
}

trait Greet {
  fn greet(self) -> String
}

trait Greet for Shape {
  fn greet(self) -> String {
    match self {
      Circle(r) -> "hi from a circle of radius {r}"
      Rect(w, h) -> "hi from a {w}x{h} rectangle"
    }
  }
}

fn main() {
  println(Circle(5.0).greet()) -- hi from a circle of radius 5
}
```

A trait declares complete signatures: every parameter but `self` has a
type annotation, and a method declared without a return type returns
`()`. An impl's method has the types its trait declares, with the impl's
type for `Self`. The impl may leave the annotations out; what it does
write must agree with the trait:

```silt
trait Scale {
  fn scale(self, by: Int) -> Int
}

-- by: Int, returns Int
trait Scale for Int {
  fn scale(self, by) {
    self * by
  }
}

-- ERROR: expected Int, got String
trait Scale for Bool {
  fn scale(self, by) {
    "no"
  }
}
```

A method's own `where` clause is part of its signature too
(`fn shw(self, x: a) -> String where a: Display`): it holds in the
default body and in every impl's body, and every call owes it.

(`Greet` is a fresh trait local to this snippet. Silt's `Display`
trait is built in and cannot be redeclared, so doc snippets that
illustrate trait *declaration* use a fresh local name; impls of the
real built-in `Display` trait look identical and are shown in
[Built-in Traits](#built-in-traits) below.)

## Supertrait Bounds

A trait can declare other traits as **supertraits** using `: Trait` after
the trait name. Implementing the subtrait then requires the type to also
implement every supertrait, and methods from the supertrait become
callable through the subtrait constraint:

```silt
trait Eq2 {
  fn eq2(self, other: Self) -> Bool
}

trait Cmp2: Eq2 {
  fn lt2(self, other: Self) -> Bool
}
```

(`Eq2`/`Cmp2` here are stand-in names so the snippet can declare the
traits without colliding with the built-in `Equal` / `Compare`, which
cannot be redefined.)

Implementing `Cmp2` on a type requires that type to also implement `Eq2`
(four of silt's six built-in traits — `Equal`, `Hash`, `Compare`,
`Display` — are structural: the language answers them for every type
made of types that have them, so for most types the obligation holds
without an impl; a type that holds a function has none of the four. Of
the other two, `Error` is implemented by hand, and `Number` is `Int` and
`Float` only).

Multiple supertraits separate with `+`:

```silt
trait Printable: Display + Hash {
  fn print_with_hash(self) -> String
}
```

### Constraint expansion

Inside a `where a: Cmp2` body, methods from `Eq2` (the supertrait)
are also callable on `a`:

```silt
fn check(a: t, b: t) -> Bool where t: Cmp2 {
  -- a.eq2(b) works because Eq2 is a supertrait of Cmp2
  match a.eq2(b) {
    true -> true
    false -> a.lt2(b)
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

A supertrait is given as many type arguments as it has parameters:

```silt
trait Holds(a) { fn held(self) -> a }
trait Bad: Holds(Int, String) { ... }
-- error: trait 'Holds' expects 1 type argument as a supertrait of 'Bad', got 2
```

Implementing a subtrait without the supertrait fails:

```silt
type MyInt { v: Int }
trait Ordered for MyInt { ... }
-- error: type 'MyInt' implements 'Ordered' but does not implement supertrait 'Equal'
-- (only when MyInt has no Equal, e.g. it has a function field)
```

## Default Methods

A trait method can carry a body inside the trait declaration itself. The
body is the **default** implementation: any impl that omits the method
has it. Impls remain free to override the default.

A default body is checked once, in the trait. There `self` is a value of
some implementing type the trait does not know, so the body may use what
the trait and its supertraits promise (their methods) and nothing else:
not a field that the implementing types happen to have.

```silt
trait Show {
  -- default body
  fn show(self) -> String {
    "default"
  }
  -- abstract method (no body)
  fn debug(self) -> String
}

type Item {
  v: Int,
}

trait Show for Item {
  -- show() is omitted: the default body is used
  fn debug(self) -> String {
    "item-debug"
  }
}

fn main() {
  println(Item { v: 1 }.show()) -- default
  println(Item { v: 1 }.debug()) -- item-debug
}
```

A trait can mix default and abstract methods freely:

- Methods with a `{ ... }` body are **defaults**. Impls may omit them; if
  they do, the default body is used.
- Methods without a body are **abstract**. Impls must provide them.

Overriding a default is just writing the method in the impl as usual:

```silt
trait Show for Item {
  fn show(self) -> String {
    "explicit-show"
  } -- overrides the default
  fn debug(self) -> String {
    "item-debug"
  }
}
```

Default bodies can call other trait methods on `self`, including
abstract ones the impl is required to provide. Dispatch routes the call
to the impl's version, so the default acts as a template that
specialises per impl:

```silt
trait Describable {
  -- abstract
  fn name(self) -> String
  -- the default uses name()
  fn greet(self) -> String {
    "hi, {self.name()}"
  }
}

type Person {
  who: String,
}

trait Describable for Person {
  fn name(self) -> String {
    self.who
  }
}

fn main() {
  println(Person { who: "alice" }.greet()) -- hi, alice
}
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
  fn empty() -> Self {
    0
  }
  fn combine(a: Self, b: Self) -> Self {
    a + b
  }
}
```

## Parameterized Implementation Targets

An impl on a parameterized record or enum can bind the target's type
parameters directly in the impl header. Lowercase names in the target's
argument list are fresh type variables scoped to every method in the impl:

```silt
type Box(T) {
  Box(T),
}

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
- **The bare form still works.** `trait X for Box { ... }` is an impl for
  every `Box(a)`, like `trait X for Box(a)` without a name for `a` —
  useful when the method bodies never observe the element type.

In the methods of an impl the header's type variables stand for any
type, as a function's do (see
[Generics](generics.md#a-type-variable-is-any-type)): a body cannot take
the `a` of `Box(a)` for an `Int`. To implement a trait for one
instantiation only, name it with an alias (`type Ints = List(Int)`,
`trait Sum for Ints`).

### Impl-level where clauses

To call a trait method on an impl-bound type variable, declare the
constraint on the **impl header** using a `where` clause. The constraint
applies to every method in the impl and is also enforced at every call
site — passing a `Box(v)` where `v`'s type does not implement the
required trait is a compile-time error:

```silt
type Box(T) {
  Box(T),
}

trait Greet {
  fn greet(self) -> String
}

trait Greet for Int {
  fn greet(self) -> String {
    "int-greet"
  }
}

trait Greet for Box(a) where a: Greet {
  fn greet(self) -> String {
    match self {
      Box(inner) -> inner.greet()
    }
  }
}

fn main() {
  println(Box(5).greet()) -- int-greet
}
```

`Box("hello").greet()` is an error: type 'String' does not implement
trait 'Greet'.

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

### An impl's method adds no bound

A bound on the impl's type variable belongs on the impl's header. An
impl's method has the signature its trait declares, `where` clause
included: it may restate a bound the trait declares for the method or
the header declares (or a supertrait of one), with the same trait
arguments, and may not add one, because a call through the
trait (`fn f(x: t) where t: Greet { x.greet() }`) knows only the trait's
signature and the header:

```silt
trait Greet for Box(a) {
  fn greet(self) -> String where a: Greet {
    match self {
      Box(inner) -> inner.greet()
    }
  }
}
-- error: method 'greet' of the impl of 'Greet' for 'Box' adds the bound
-- `a: Greet`, which the trait does not declare for it
```

Field access on a type-var field in a record works the same way:

```silt
type Cell(T) {
  value: T,
}

trait Peek {
  fn peek(self) -> Int
}

trait Peek for Int {
  fn peek(self) -> Int {
    self
  }
}

trait Peek for Cell(a) where a: Peek {
  fn peek(self) -> Int {
    self.value.peek()
  }
}
```

## Parameterized Traits

A trait can take type parameters of its own, letting the same trait name
represent a family of related interfaces:

```silt
trait From(a) {
  fn from(source: a) -> Self
}

type Celsius {
  c: Float,
}

type Fahrenheit {
  f: Float,
}

trait From(Celsius) for Fahrenheit {
  fn from(source: Celsius) -> Self {
    Fahrenheit { f: source.c * 1.8 + 32.0 }
  }
}
```

Parameterized traits may have supertraits (`trait Child(a): Parent(a)`) and
`where` clauses on both the trait declaration and its methods. The full
specification — including how trait parameters flow through `where` clauses
and how multiple impls with different arguments coexist — lives in
[Generics — Parameterized trait declarations](generics.md#parameterized-trait-declarations).

## Static Trait Methods

Some trait methods take no `self` and only return `Self` — constructors like
`Default::default()` or `Monoid::empty()`. Invoke these by calling on a
**type descriptor**: either a bare type name (`Int.empty()`) or a `type a`
parameter inside a generic function (`a.empty()`):

```silt
trait Monoid {
  fn empty() -> Self
  fn combine(a: Self, b: Self) -> Self
}

trait Monoid for Int {
  fn empty() -> Self {
    0
  }
  fn combine(a: Self, b: Self) -> Self {
    a + b
  }
}

fn main() {
  let zero = Int.empty() -- concrete dispatch
  let five = Int.combine(2, 3)
  println("{zero} {five}")
}
```

A method without `self` is called on a type, never on a value:
`5.empty()` is an error that names the call to write (`Int.empty()`). An
impl writes the parameters its trait declares: one that adds or drops
`self` (or any other parameter) is an error at the impl.

Dispatch is by the descriptor's carried type name — `Int.empty()` resolves
to the `Monoid` impl for `Int`. Inside a generic function with
`type a` and `where a: Monoid`, `a.empty()` dispatches to whichever impl
matches the concrete type passed in. See
[Generics — Trait methods on types](generics.md#trait-methods-on-types) for
the full rules.

## Method References

Trait method names are first-class callable values: `TypeName.method` produces
a function you can pass directly to a higher-order combinator. This avoids
writing a one-line closure around `x.method()`:

```silt
import list

type Celsius {
  c: Float,
}

trait Display for Celsius {
  fn display(self) -> String {
    "{self.c}°C"
  }
}

fn main() {
  let temps = [Celsius { c: 20.0 }, Celsius { c: 25.5 }]
  let rendered = list.map(temps, Celsius.display)
  list.each(rendered) { line -> println(line) }
}
```

`value.method` without a call is not a function: a method is called.
Write `Type.method`, or a closure, `{ x -> x.method() }`.

## Calls on a value of unknown type

In a function whose parameter has no annotation, `x.m(..)` and `x.f` are
decided by what is written, not by which names happen to exist:

- `x.f` is a field: `fn name_of(p) { p.name }` takes any record with a
  field `name`, whatever methods traits declare.
- `r.f(..)` on a record with a field `f` calls the function the field
  holds, even if a trait has a method `f` for the record's type.
- `x.m(..)` means the one trait in sight that declares a method `m`, and
  bounds `x` by it: `fn g(x) { x.greet() }` is `fn g(x: a) -> String
  where a: Greet`. If no trait declares `m`, `x` is a record whose field
  `m` holds a function. If several traits declare `m`, annotate `x`.

When the type of `x` is decided later in the same definition (a closure's
parameter by the call it is passed to), the call is checked against that
type.

## Built-in Traits

silt ships **six** built-in traits. Four of them — `Equal`, `Hash`,
`Compare`, `Display` — are **structural**: the language answers them
itself, for every type made of types that have them (what that means
for each kind of type is listed below). Of the four only `Display` may
be written by hand. `Error` is implemented by hand. `Number` is the
types arithmetic is on, `Int` and `Float`, and nothing else.

| Trait     | Purpose                          | Who has it | An impl may be written |
|-----------|----------------------------------|------------|------------------------|
| `Display` | Convert to human-readable string | by structure | yes: it replaces the structural one |
| `Equal`   | Equality comparison              | by structure | no (an error) |
| `Hash`    | Hash value for maps/sets         | by structure | no (an error) |
| `Compare` | Order comparison; includes `Equal` | by structure | no (an error) |
| `Error`   | Error reporting (`message()`)    | types with an impl | yes |
| `Number`  | `+ - * / %` and unary `-`        | Int and Float only | no |

Operators are these traits: `==` and `!=` need `Equal`, `<` `>` `<=`
`>=` need `Compare`, interpolation and `println` need `Display`,
arithmetic needs `Number`. A function without annotations is general
over them (`fn add(a, b) { a + b }` is `(a, a) -> a where a: Number`);
with annotation variables it declares them:

```silt
fn largest(x: a, y: a) -> a where a: Compare {
  match x < y {
    true -> y
    false -> x
  }
}
```

`?` and a record update (`r.{ f: e }`) are not general: the value they
apply to needs a type the definition decides, or an annotation. `?`
takes it from the return type of the function it is in, when that is
known: `fn step(x) -> Result(Int, String) { Ok(x? + 1) }`.

The structural `Display` formats in constructor syntax: `Circle(5)`
for a variant, `Point {x: 1, y: 2}` for a record, its fields in the
order the type declares them (`{x: 1, y: 2}` for an anonymous record,
its fields in the order of their names). Write your own
`trait Display for T` to replace it, in the module that declares `T`
(see [Where an Impl Is Written](#where-an-impl-is-written)): a
`Display` impl for a type of another module, or for a built-in type, is
an error. A type that holds a function has no structural `Display`, and
may be given a written one. There is one `Display`: `println`, `print`, interpolation,
`.display()`, `string.from` and a panic's message all show a value the
same way, and all use the impl you wrote, wherever a value of the type
is inside what is shown (`println([t])`, `"{Some(t)}"`, a field of a
record).

```silt
type Temp {
  degrees: Int,
}

trait Display for Temp {
  fn display(self) -> String {
    "{self.degrees}C"
  }
}

fn main() {
  let t = Temp { degrees: 21 }
  println(t) -- 21C
  println([t, t]) -- [21C, 21C]
  println("{Some(t)}") -- Some(21C)
}
```

`io.inspect` and the texts of failed assertions (`test.assert_eq` and
the like) are for the person debugging: they write the structure of a
value in silt syntax, strings quoted, and do not call written `Display`
impls (`io.inspect(t)` is `Temp {degrees: 21}`). They write a record's
fields in the same order as `println`.

`Equal`, `Hash` and `Compare` are **sealed**: they always come
from a type's structure (a type gets them when every field
supports them), and `==`, `<`, `.equal()`, `.compare()`, `.hash()` and
map keys use exactly that structure. A record is ordered by its fields
in the order the type declares them, an enum by its variants in the
order it declares them and then by their payloads. A
hand-written `trait Equal for T`, `trait Compare for T` or
`trait Hash for T` is an error:

```silt
type Version { major: Int, minor: Int }
trait Compare for Version { ... }
-- error: trait 'Compare' cannot be implemented by hand: it is derived
-- structurally for every type whose fields support it — remove this
-- impl; Equal, Compare and Hash are derived
```

What "every field supports them" means, for `Equal`, `Hash`, `Compare`
and for `Display` where no impl is written:

- `Int`, `Float`, `String`, `Bool` and `()` have all four.
- A function has none. A channel has `Equal` only (it is equal to
  itself).
- A list and a tuple have what their parts have (a tuple is ordered
  part by part). A map and a set have `Equal`, `Hash` and `Display`
  when their parts do, and no `Compare`.
- A record or an enum has a trait when every field and payload does, at
  the type's arguments: `Option(Int)` has `Compare`, `Option(Fn(Int) ->
  Int)` has none of them. A closed anonymous record (`{a: Int}`) has
  `Equal`, `Hash` and `Display` when its fields do.
- A type variable has what its bound says (`where a: Compare`).
  `Compare` includes `Equal`: what is ordered can be compared with `==`.

Declaring a type never fails for lack of one of these traits: a type may
hold a function anywhere. The error is at the use that needs the trait
(`println`, `==`, a map key), and names the way down to the part that
lacks it.

The standard library declares what it asks of its arguments the same
way: `list.sort` needs `Compare` of the elements, `list.contains`
`Equal`, map keys and set elements `Hash` (so do `#{..}` and `#[..]`
literals), `println` and `string.from` `Display`.

```silt
type Job { name: String, run: Fn(Int) -> Int }
println(Job { name: "j", run: { n -> n } })
-- error: type 'Job' does not implement trait 'Display': field 'run' is
-- of type 'Fn(Int) -> Int', which does not
```

The `Error` trait has supertrait `Display` and one method,
`message(self) -> String`. Each stdlib error enum (`IoError`,
`JsonError`, `HttpError`, …) implements it explicitly, and user code
can implement it on its own error types.

## Where an Impl Is Written

An impl is written in the module that declares its trait, or in the
module that declares its type. Anywhere else it is an error.

A built-in trait and a built-in type are declared in no module, so:

- an impl of `Display` or `Error` for a type goes in the module that
  declares the type;
- an impl for `Int`, `String`, `List(a)`, `Option(a)` or another
  built-in type goes in the module that declares the trait;
- no module may write an impl of a built-in trait for a built-in type.

For an impl with a parameterized target (`trait Area for Box(a)`), the
type is the one named first, `Box`.

Two things follow. A type has at most one impl of a trait in a whole
program, and every module sees the same one: no two modules can each
write it. And the impl is there wherever it is called: a module that can
call a method of a trait on a value of a type imports the module of the
trait and the module of the type (directly, or through the modules it
imports), and one of those two holds the impl. A value is shown, and a
method answers, the same way in every module.

A module calls the methods of the traits whose modules it imports,
directly or through the modules it imports, and of the built-in traits.

### In the trait's module

```silt
trait Greet {
  fn greet(self) -> String
}

trait Greet for List(a) {
  fn greet(self) -> String {
    "a list"
  }
}
```

### In the type's module

```silt
type Color {
  Red,
  Green,
  Blue,
}

trait Display for Color {
  fn display(self) -> String {
    "a color"
  }
}
```

A module that imports `Color` cannot write this impl: it belongs beside
the type. The same holds for a trait and a type of two other modules:

```silt
-- main.silt
import shapes.{ Square }
import traits.{ Area }

trait Area for Square { -- error
  fn area(self) -> Int {
    self.side * self.side
  }
}
```

```
the impl of trait 'Area' for type 'Square' is written in module 'main',
which declares neither the trait nor the type; it may be written in
module 'traits', which declares the trait, or in module 'shapes', which
declares the type
```

### Neither has a module

```silt
trait Display for List(a) { -- error
  fn display(self) -> String {
    "stolen"
  }
}
```

```
the impl of trait 'Display' for type 'List' is written in module 'main',
which declares neither the trait nor the type; both are builtin, so no
module may write it
```

To add behaviour to a built-in type, wrap it in a type of your own or
declare your own trait and implement that instead.

In the REPL, the entries of a session count as one module: an impl for
a type of an earlier entry may be written in a later one.

## Where Clauses

Constrain generic parameters to types implementing a trait. Where clauses
**must** use explicit type annotations:

```silt
import list

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
