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
  println(Greet.greet(Circle(5.0))) -- hi from a circle of radius 5.0
}
```

A method that takes a value of the implementing type (`self`, or a
parameter of type `Self`) is called through its trait, as a module's
function is: `Greet.greet(c)`, `c |> Greet.greet`,
`list.map(shapes, Greet.greet)`. The call reaches the impl for the type
of that argument. A trait of another module is named as its other items
are: `shapes.Area.area(s)` after `import shapes`, or `Area.area(s)` after
`import shapes.{ Area }`. A dot after a value always names a field (see
[A dot after a value](#a-dot-after-a-value)).

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

A record may have a field and an impl a method of one name: a dot after
a value is always the field, and a trait's method is called through its
trait. `job.run(5)` calls the function the field `run` holds,
`Runner.run(job, 5)` the method of the impl.

```silt
type Job {
  name: String,
  run: Fn(Int) -> Int,
}

trait Runner {
  fn run(self, x: Int) -> Int
}

trait Runner for Job {
  fn run(self, x: Int) -> Int {
    x + 100
  }
}

fn main() {
  let job = Job { name: "double", run: { x -> x * 2 } }
  println(job.run(5)) -- 10
  println(Runner.run(job, 5)) -- 105
}
```

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

Inside a `where t: Cmp2` body, a value of type `t` is also given to the
methods of `Eq2` (the supertrait), each called through its own trait:

```silt
fn check(a: t, b: t) -> Bool where t: Cmp2 {
  -- Eq2.eq2(a, b) works because Eq2 is a supertrait of Cmp2
  match Eq2.eq2(a, b) {
    true -> true
    false -> Cmp2.lt2(a, b)
  }
}
```

The expansion is transitive: with `trait C: B { ... }` and
`trait B: A { ... }`, a `where x: C` constraint lets `x` be given to the
methods of `A`, `B`, and `C`, and implementing `C` on a type requires
impls of `A`, `B`, and `C`.

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

An impl for a parameterized type has the supertrait at its own type,
with what its header gives the type's variables: a `Box(a)` has `Equal`
when its `a` does, so the header says so:

```silt
trait Same: Equal {
  fn same(self, other: Self) -> Bool {
    self == other
  }
}

type Box(a) {
  Bx(a),
}

trait Same for Box(a) {}
-- error: type 'Box(a)' implements 'Same' but does not implement supertrait 'Equal': 'a' needs 'Equal'
-- help: add `where a: Equal` to the impl
```

`trait Same for Box(a) where a: Equal {}` is the impl.

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
  println(Show.show(Item { v: 1 })) -- default
  println(Show.debug(Item { v: 1 })) -- item-debug
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

Default bodies can call other methods of the trait with `self`,
including abstract ones the impl is required to provide. Dispatch routes the call
to the impl's version, so the default acts as a template that
specialises per impl:

```silt
trait Describable {
  -- abstract
  fn name(self) -> String
  -- the default uses name()
  fn greet(self) -> String {
    "hi, {Describable.name(self)}"
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
  println(Describable.greet(Person { who: "alice" })) -- hi, alice
}
```

Defaults compose with supertraits: a default body may call a supertrait
method with `self`, and the obligation that the supertrait be implemented
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
type Box(a) {
  Box(a),
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

- **An impl is for a declared type.** `trait X for a` is an error
  (`trait impl target 'a' is not a declared type`): a function bounded by
  the trait says "every type that has it", `fn x(v: a) where a: Show`.
- **Only lowercase binders.** `trait X for Box(Int)` is a parse error —
  silt has no specialization.
- **Binders must be distinct.** `trait X for Pair(a, a)` is a parse error.
- **Arity must match the target.** `trait X for Box(a, b)` on the 1-param
  `Box(a)` is a type error.
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

To give a value of an impl-bound type variable to a trait's method,
declare the constraint on the **impl header** using a `where` clause. The constraint
applies to every method in the impl and is also enforced at every call
site — passing a `Box(v)` where `v`'s type does not implement the
required trait is a compile-time error:

```silt
type Box(a) {
  Box(a),
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
      Box(inner) -> Greet.greet(inner)
    }
  }
}

fn main() {
  println(Greet.greet(Box(5))) -- int-greet
}
```

`Greet.greet(Box("hello"))` is an error: type 'String' does not
implement trait 'Greet'.

Several bounds of one variable are joined by `+`, as in a function's
`where`:

```silt
trait Greet for Box(a) where a: Greet + Loud {
  fn greet(self) -> String {
    match self {
      Box(inner) -> "{Greet.greet(inner)}-{Loud.loud(inner)}"
    }
  }
}
```

An impl of `Display`, `json.Encode`, `json.Decode` or `Error` asks only
builtin traits (`Display`, `Equal`, `Compare`, `Hash`, `json.Encode`,
`json.Decode`, `Error`) of its type's variables. The runtime runs such
an impl itself (`println` shows the value, `json.stringify` encodes it),
with no caller to give what a bound of a program's own trait needs.
Show or encode the part through the builtin trait instead:

```silt
trait Display for Box(a) where a: Display {
  fn display(self) -> String {
    match self {
      Box(inner) -> "box {inner}"
    }
  }
}
```

### An impl's method adds no bound

A bound on the impl's type variable belongs on the impl's header. An
impl's method has the signature its trait declares, `where` clause
included: it may restate a bound the trait declares for the method or
the header declares (or a supertrait of one), with the same trait
arguments, and may not add one, because a call through the
trait (`fn f(x: t) where t: Greet { Greet.greet(x) }`) knows only the trait's
signature and the header:

```silt
trait Greet for Box(a) {
  fn greet(self) -> String where a: Greet {
    match self {
      Box(inner) -> Greet.greet(inner)
    }
  }
}
-- error: method 'greet' of the impl of 'Greet' for 'Box' adds the bound
-- `a: Greet`, which the trait does not declare for it
```

Field access on a type-var field in a record works the same way:

```silt
type Cell(a) {
  value: a,
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
    Peek.peek(self.value)
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
`where` clauses on both the trait declaration and its methods.

A type has one impl of a trait for each trait argument: `trait
From(Celsius) for Fahrenheit` and `trait From(Kelvin) for Fahrenheit`
are two impls, and a use reaches the one its trait argument says. The
argument is the type the use gives or asks: `Fahrenheit.from(c)` with a
`Celsius` takes the first, and `result.map_err(r, AppError.from)` takes
the impl for the error type of `r`. Where nothing decides the trait
argument of a use, which impl it means is unknown, and that is an error,
even where the type has only one impl of the trait (annotate the type it
gives, or call it where a bound decides it: `where t: From(Celsius)`).
Impls are told apart by their whole trait arguments: `From(List(Int))` and
`From(List(String))` for one type are two impls, and so are
`From((Int, Int))` and `From((Int, Int, Int))`. Two impls whose trait
arguments could be one type are a duplicate: an impl whose trait argument
is a type variable (`trait From(a) for Fahrenheit`) is for every argument,
the type's only impl of the trait, and `From(List(a))` meets
`From(List(Int))`. A use reaches the one impl whose trait arguments its own
could be; while two still could (`From(List(_))` with both impls above),
it waits for the rest of its definition to decide.

`From` is no builtin trait: a program declares it, and its impls, as above.
It converts errors to a program's own error type where one function calls
into several modules, each with its error type:

```silt
import io
import json
import result

type Config {
  name: String,
}

type AppError {
  IoProblem(io.IoError),
  JsonProblem(json.JsonError),
}

trait From(e) {
  fn from(source: e) -> Self
}

trait From(io.IoError) for AppError {
  fn from(e: io.IoError) -> AppError {
    IoProblem(e)
  }
}

trait From(json.JsonError) for AppError {
  fn from(e: json.JsonError) -> AppError {
    JsonProblem(e)
  }
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = result.map_err(io.read_file(path), AppError.from)?
  let config = result.map_err(json.parse(raw, Config), AppError.from)?
  Ok(config)
}
```

`?` only propagates the error; the conversion is the call written at each
place, `AppError.from`.

The full specification — including how trait parameters flow through
`where` clauses — lives in
[Generics — Parameterized trait declarations](generics.md#parameterized-trait-declarations).

## Methods That Make a Value

Some trait methods take no value of the type and only make one —
constructors like `default()` or `empty()`. A call through the trait
could not say which impl it means, so these are called on a **type**:
either a bare type name (`Int.empty()`) or, inside a generic function, a
type variable with the bound (`a.empty()` where `a: Monoid`). A method
that takes a value of the type is called through its trait
(`Monoid.combine(2, 3)`), never on a type:

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
  let five = Monoid.combine(2, 3)
  println("{zero} {five}")
}
```

`5.empty()` is an error: a dot after a value names a field, and the
help names the call to write (`Int.empty()`). `Int.combine(2, 3)` is an
error too: `type 'Int' has no method 'combine'`, with the help to call
it as `Monoid.combine(x, ..)`. Through a type only the methods that make
a value are candidates: of two traits that give a type a method of one
name, one that makes a value and one that takes one, `T.m(..)` is the
first; two that make one are ambiguous. An impl writes the parameters
its trait declares: one that adds or drops `self` (or any other
parameter) is an error at the impl.

Dispatch is by the type — `Int.empty()` resolves to the `Monoid` impl
for `Int`. Inside a generic function with `type a` and
`where a: Monoid`, `a.empty()` dispatches to whichever impl matches the
concrete type passed in. See
[Generics — Trait methods on types](generics.md#trait-methods-on-types) for
the full rules.

A type with a variant of its own name (`type Box(a) { Box(a), Empty }`)
is the type where a type is meant: `Box.default()` calls the method of
its impl, and `make(Box)` gives the type to a `type t` parameter;
`Box(1)` is the variant.

## Method References

A method named through its trait is a function value, as a module's
function is: `Trait.method` can be passed directly to a higher-order
combinator. Where the type of its argument is known the value is the
method of that impl; in generic code it is the method of the impl the
function was given for its bound (see [How a Method Is
Found](#how-a-method-is-found)).

```silt
import list

type Celsius {
  c: Float,
}

trait Describe {
  fn describe(self) -> String
}

trait Describe for Celsius {
  fn describe(self) -> String {
    "{self.c}°C"
  }
}

fn main() {
  let temps = [Celsius { c: 20.0 }, Celsius { c: 25.5 }]
  let rendered = list.map(temps, Describe.describe)
  list.each(rendered) { line -> println(line) }
}
```

A method that makes a value is taken through its type
(`list.map(codes, AppError.from)`).

## A dot after a value

A dot after a value names a field, whatever methods traits declare:

- `p.name` is the record's field; `r.f(..)` calls the function the field
  `f` holds.
- In a function whose parameter has no annotation, `fn name_of(p) {
  p.name }` takes any record with a field `name`.
- A trait's method is called through the trait: `fn g(x) {
  Greet.greet(x) }` is `fn g(x: a) -> String where a: Greet`, the bound
  coming from the trait the call names.

A dot that names nothing is an error that says what the value's type
lacks, with the call to write where the name is a trait's method or a
function of the type's builtin module:

```silt
import list

trait Greet {
  fn greet(self) -> String
}

trait Greet for Int {
  fn greet(self) -> String {
    "hi"
  }
}

fn shown(n: Int, xs: List(Int)) {
  println(n.frobnicate()) -- error: type 'Int' has no field 'frobnicate'
  println(n.greet())
  -- error: type 'Int' has no field 'greet'
  -- help: `greet` is a method of trait `Greet`: call it as `Greet.greet(n)`
  println(xs.length())
  -- error: type 'List(Int)' has no field 'length'
  -- help: `length` is a function of module `list`: call it as `list.length(xs)`
}
```

When the type of `x` is decided later in the same definition (a closure's
parameter by the call it is passed to), the field is checked against that
type.

## How a Method Is Found

The impl a call of a method of a trait your program declares reaches is
chosen by the types, where the program is checked, never by a value
where it runs:

- On a type the checker knows (`Greet.greet(K)`, `Int.empty()`), the
  call is the method of that type's impl.
- In a function with a bound (`fn hello(x: a) -> String where a: Greet`),
  each call of the function passes the impl its types choose:
  `hello(K)` passes `Greet for K`, and every `Greet.greet(x)` in the body
  calls that impl's method. An impl whose header asks a bound
  (`trait Greet for Box(a) where a: Greet`) is passed with the impl for
  the type inside: `hello(Box(K))` passes `Greet for Box(a)` with
  `Greet for K`.
- A method or a function with a bound taken as a value
  (`list.map(xs, Greet.greet)`, `list.map(xs, hello)`) keeps the impls
  it was given, wherever it goes: into a task, through a channel.
- A `let` that is general in a bound (`let shout = { x -> "{Greet.greet(x)}!" }`)
  stays general: its value is made again at each use, with the impls of
  that use, so `shout(K)` and `shout(J)` are each their type's.

The built-in traits (`Equal`, `Compare`, `Hash`, `Display`,
`json.Encode`, `json.Decode`, `Error`) are found by the value where the
code runs: `println` shows a `Pt` inside a list with `Pt`'s `Display`,
at any depth. An impl of `Display`, `json.Encode`, `json.Decode` or
`Error` asks only built-in traits of its type's variables (see
[Impl-level where clauses](#impl-level-where-clauses)).

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
| `Error`   | Error reporting (`Error.message(e)`) | types with an impl | yes |
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

The structural `Display` formats in constructor syntax: `Circle(5.0)`
for a variant, `Point {x: 1, y: 2}` for a record, its fields in the
order the type declares them (`{x: 1, y: 2}` for an anonymous record,
its fields in the order of their names). Write your own
`trait Display for T` to replace it, in the module that declares `T`
(see [Where an Impl Is Written](#where-an-impl-is-written)): a
`Display` impl for a type of another module, or for a built-in type, is
an error. A type that holds a function has no structural `Display`, and
may be given a written one. There is one `Display`: `println`, `print`, interpolation,
`string.from` and a panic's message all show a value the same way, and all use the impl you wrote, wherever a value of the type
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
supports them), and `==`, `<`, `Compare.compare`, `Hash.hash` and map
keys use exactly that structure. `Display`'s `display` and `Equal`'s
`equal` are not called by a program: interpolation and `string.from`,
and `==`, do what they would. A record is ordered by its fields
in the order the type declares them, an enum by its variants in the
order it declares them and then by their payloads. A
hand-written `trait Equal for T`, `trait Compare for T` or
`trait Hash for T` is an error (what to write instead is in
[Equality and order idioms](#equality-and-order-idioms)):

```silt
type Version { major: Int, minor: Int }
trait Compare for Version { ... }
-- error: trait 'Compare' cannot be implemented by hand: it is derived
-- structurally for every type whose fields support it — remove this
-- impl; Equal, Compare and Hash are derived
-- help: to compare, order or hash by something else (a key, a
-- normalised value), see "Equality and order idioms" in
-- docs/language/traits.md
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
literals), `println` and `string.from` `Display`, `json.stringify` and
`json.pretty` `json.Encode`. That one is a trait of the json module: a
type has it by its structure when each of its parts has a json form
(`Bytes`, a set, a function, a channel and a handle have none; a map
asks it of its values only), a `trait json.Encode for T` in the module
of `T` replaces it as a written `Display` impl does, and a program names
it `json.Encode` (see [json](../stdlib/json.md#jsonencode)). Likewise
`json.parse`, `json.parse_list`, `json.parse_map` and `json.decode`
need `json.Decode` of the type they decode into: a type has it when the
json decoders decode it (see [json](../stdlib/json.md#jsondecode)). A
function with a `type a` parameter that hands it to one writes `where a:
json.Decode`; only a function with no annotation at all has it inferred.

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

### Equality and order idioms

Since `==`, `<` and map keys always use a type's structure, a type
compares by something else through what it holds or how it is used,
not through an impl:

- **Normalise once, in a constructor function.** A value that has
  several spellings of one meaning (an e-mail address in any case, a
  path with a trailing slash) is made by one function that writes the
  one spelling. Its `==`, its hash and its order are then the meaning's.
- **Key a map by an id**, not by the record: `map.set(users, u.id, u)`.
  Two versions of one record (an old and a new name) are one entry, and
  the record may hold what has no `Hash`.
- **Key a record that holds a function by its name.** A function has no
  `Equal` or `Hash`, so neither has a record with one: keep such records
  in a map from their names, and compare the names.
- **Sort by a key** with `list.sort_by`: a tuple key sorts by its parts
  in turn, `|> list.reverse` sorts descending, and `list.sort_with`
  takes a comparison of two elements (`Compare.compare(b, a)` for
  descending order).
- **Find duplicates by a key** with `list.unique_by`, which keeps the
  first element of each key.
- **Order versions as records.** A record compares field by field in the
  order the type declares them, so `Version { major, minor, patch }`
  orders versions as versions are ordered, with `<`, `list.sort` and
  map keys, and needs no impl.

```silt
import list
import map
import string

type Email {
  address: String,
}

fn email(text: String) -> Email {
  Email { address: string.to_lower(string.trim(text)) }
}

type Version {
  major: Int,
  minor: Int,
  patch: Int,
}

type Command {
  name: String,
  run: Fn(Int) -> Int,
}

fn main() {
  println(email(" Ann@Example.com") == email("ann@example.com")) -- true

  let v1 = Version { major: 1, minor: 10, patch: 0 }
  let v2 = Version { major: 1, minor: 9, patch: 3 }
  println(v2 < v1) -- true

  let commands = [
    Command { name: "double", run: { n -> n * 2 } },
    Command { name: "negate", run: { n -> -n } },
  ]
  let by_name = map.from_entries(list.map(commands) { c -> (c.name, c) })
  match map.get(by_name, "double") {
    Some(c) -> println(c.run(21)) -- 42
    None -> println("no such command")
  }

  let words = ["Apple", "avocado", "Banana", "apple"]
  let firsts = list.unique_by(words) { w -> string.to_lower(string.slice(w, 0, 1)) }
  println(firsts) -- [Apple, Banana]
  let ranked = list.sort_by(words) { w -> (string.length(w), w) } |> list.reverse
  println(ranked) -- [avocado, Banana, apple, Apple]
}
```

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
write it. And the impl is there wherever it is called: a call names the
trait, so its module is in the program, and so is the module of the
type of a value the call is given; one of those two holds the impl. A
value is shown, and a method answers, the same way in every module.

A module calls a trait's methods through the trait's name, which it
names as any item: a trait it declares, one it imports by name
(`import shapes.{ Area }`), one of a module it imports
(`shapes.Area.area(s)`), or a built-in trait (`Compare`, `Hash`,
`Error`; `json.Encode` after `import json`).

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
  items |> list.each { item -> println("{item}") }
}

-- ERROR: 'a' is unbound -- no annotation on x
fn f(x) where a: Display {
  println("{x}")
}
```

The form `fn f(x) where a: Display` is an error because the compiler cannot
determine which parameter `a` refers to.

Multiple trait bounds of one variable are joined by `+`, and `,`
separates variables:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash {
  ...
}
```

A variable is named once: `where a: Equal, a: Hash` is an error, whose
help writes `where a: Equal + Hash`.
