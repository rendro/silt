---
title: "Generics"
section: "Language"
order: 8
---

# Generics

Silt is a statically-typed language with full parametric polymorphism. Type
variables are inferred by the compiler using Hindley–Milner inference with
let-polymorphism; you rarely need to declare them. When you do, the syntax
is a single lowercase identifier — no angle brackets, no special binder
keyword on functions.

The guiding principle: **generics should be invisible when they can be, and
unambiguous when they must be visible**. Readers should never have to ask
"where did this type variable come from?"

## The mental model

1. **Lowercase identifiers in type positions are type variables.**
   One written with a type argument (`m(Int)`) is a type constructor
   variable. Uppercase identifiers are concrete types or type
   constructors.
2. **Type variables bind at their first appearance** in a signature,
   reading left to right.
3. **Every type variable must be anchored to a parameter.** Either it
   appears inside the type of a regular parameter, or it is introduced
   explicitly as a `type a` parameter (see [Return-polymorphic functions](#return-polymorphic-functions)).
4. **The compiler does the rest.** Polymorphism, specialisation,
   constraint resolution, and instantiation at call sites are all
   inferred. There is no `fn<T>` binder syntax and no turbofish.

```silt
fn map(xs: List(a), f: Fn(a) -> b) -> List(b) { ... }
--             ↑              ↑         ↑
--        a binds here   b binds here   both already in scope
```

## Generic type declarations

Type parameters are declared in parentheses after the type name:

```silt
type Option(a) {
  Some(a),
  None,
}

type Result(a, e) {
  Ok(a),
  Err(e),
}

type Pair(a, b) {
  first: a,
  second: b,
}

type Tree(a) {
  Leaf,
  Node(Tree(a), a, Tree(a)),
}
```

Parameters are lowercase and distinct. They are in scope throughout the
declaration body, including in constructor argument types, record
fields, and recursive self-references. Every type variable of the body
is one of them: `type Maybe { Some(a), None }` is an error ("type
variable 'a' is not a parameter of 'Maybe'"), and so is `type Cell(T)`.
A record type in a declaration is closed: it has no `...r` row.

At use sites, types are applied positionally: `Option(Int)`,
`Result(String, ParseError)`, `Pair(User, List(Role))`.

Constructors are automatically polymorphic:

```silt
let a = Some(42)           -- Option(Int)
let b = Some("hello")      -- Option(String)
let c = Ok(User { ... })   -- Result(User, _)
```

## Generic functions

Write functions without declaring type parameters — the compiler infers
them from parameter annotations:

```silt
fn identity(x: a) -> a {
  x
}

fn swap(pair: (a, b)) -> (b, a) {
  let (x, y) = pair
  (y, x)
}

fn compose(f: Fn(b) -> c, g: Fn(a) -> b) -> Fn(a) -> c {
  { x -> f(g(x)) }
}
```

Every lowercase name in a type annotation that is not already bound
becomes a fresh type variable at the binding point. Subsequent uses of
the same name refer to the same variable.

A type variable may not be spelled like a known type in lower case:
`fn f(x: int)` is an error: "unknown type 'int' — did you mean `Int`?".
(Type names always start with a capital letter, so such a name is a typo.
One-letter names such as `a`, `e` or `t` are always type variables.)

Type variables may appear:

- As a full parameter type: `x: a`
- Inside a type constructor: `xs: List(a)`, `m: Map(k, v)`
- Inside a function type: `f: Fn(a) -> b`
- Applied, as a type constructor variable: `xs: m(Int)` (see
  [Functors](functors.md))
- In the return type; one a `where` clause names, only if a parameter binds
  it, or a parameter of a function the function returns

### The binding rule

Every type variable that a `where` clause names must appear in at least one
**input position**: a regular parameter's type, a `type a` parameter (see
below), or a parameter of a function type the function returns. A type
variable that appears only in the results of the return type and that a
bound names, or only in a `where` clause, is a compile-time error:

```silt
-- ERROR: 'a' only appears in the return type, and a bound names it
fn zero() -> a where a: Monoid { ... }

-- ERROR: 'a' only appears in a where clause
fn parse(s: String) -> Int where a: Parse { ... }
```

The rule is the same for a signature the checker infers: `fn cx(w) {
Conv.conv(w) }` has the type `Fn(a) -> b where a: Conv(b)` and is refused
as its written form is. Which impl a bound picks is always decided by what
a caller passes, never by what the caller expects back. So
`fn shown() -> Fn(a) -> String where a: Display` is accepted: whoever calls
the returned function passes the `a`.

A return-only type variable that no bound names is allowed, written or
inferred: `fn empty() -> List(a) { [] }` gives each caller the list it asks
for. A function whose body never returns (`fn fail() { panic("failed") }`)
is inferred to return `Never`, as `fn fail() -> Never` writes it.

A type constructor variable follows the same rule: `m` in `xs: m(Int)`
appears in a parameter's type, or is a `type m` parameter.

### A type variable is any type

Inside the function a type variable stands for a type the function does
not know: the body must work for every type the caller may choose. It
cannot treat an `a` as an `Int`, return a `b` where it promised an `a`,
or ask of it a trait no `where` clause promises:

```silt
fn one(x: a) -> a { 1 }                    -- ERROR: expected a, got Int
fn second(x: a, y: b) -> a { y }           -- ERROR: expected a, got b
fn show(x: a) -> String { "{x}" }          -- ERROR: `a` is not known to implement Display
fn show(x: a) -> String where a: Display { "{x}" }   -- ok
```

An annotation in the body that writes one of the signature's variables
means the same variable (`let first: a = x`). A variable that a `let`
annotation introduces itself is any type for that `let`:
`let id: Fn(b) -> b = { y -> y }` is general in `b`, and
`let n: b = 1` is an error.

Functions that call each other may leave their return types out and
still annotate parameters with variables: inside the group two such
variables of two functions can be the same one, and each function stays
general in it.

A function without annotations is not restricted this way: its type is
inferred from its body, bounds included, so `fn wrap(x) { show(x) }` has
the type `Fn(a) -> String where a: Display`. Functions that call each
other are inferred together, and inside such a group each has one type;
annotate a function to call it at two types from within its own group.

## Where clauses

Constrain a type variable to types implementing a trait with `where`:

```silt
fn sort(xs: List(a)) -> List(a) where a: Compare {
  ...
}
```

Multiple bounds on one variable separate with `+`:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash { ... }
```

Bounds on multiple variables separate with `,`:

```silt
fn merge(a: Map(k, v), b: Map(k, v)) -> Map(k, v)
  where k: Hash + Equal, v: Display
{ ... }
```

`+` joins the bounds of one variable and `,` separates variables. A
variable is named once: `where a: Equal, a: Hash` is an error, whose
help writes `where a: Equal + Hash`.

`where` clauses are enforced at every call site. Passing a value whose
element type doesn't satisfy the constraint is a compile-time error
that names both the call and the missing impl.

### Constraints on impl targets

Traits can be implemented on parameterized types with constraints on the
bound parameters. See [Traits — Parameterized Implementation Targets](traits.md#parameterized-implementation-targets)
for the full specification. A constraint on the impl header applies to
every method in the impl and is checked at every call site.

## Return-polymorphic functions

Some functions are genuinely polymorphic in their return type with no
corresponding input — `default`, `empty`, `parse`, `decode`. For these,
silt uses an explicit `type` parameter: a function argument whose value
is a type.

```silt
fn default(type a) -> a where a: Default {
  a.default()
}

type DecodeError {
  Malformed(String),
}

fn parse(body: String, type a) -> Result(a, DecodeError) where a: Parse {
  a.parse(body)
}

fn try_from(x: a, type b) -> b where a: Convertible(b) {
  Convertible.convert(x)
}
```

Inside the body, the `type a` parameter doubles as a dispatch target for
the trait methods that make a value. `a.default()` invokes the `default` method of whichever
`Default` impl matches the concrete type passed in — see
[Trait methods on types](#trait-methods-on-types) below.
`try_from` shows the companion pattern: a parameterized trait
(`Convertible(b)`) where the trait's own parameter `b` is bound via the
`where` clause and flows into the return type.

At the call site, the type is passed like any other argument:

```silt
let zero = default(Int)
let todo = json.parse(body, Todo)
let n = try_from("42", Int)
```

The type passed is any type written in full: a type name, a type
applied to types (`make(List(Int))`, `make(Map(String, Int))`), a tuple
of types (`make((Int, String))`), or an alias of one, which stands for
the type it names. A type with an argument left open (`List`) is no
type to decode into. Passing a lowercase type variable only works when
that variable is in scope at the call site, and a function passes its
own on inside a type the same way (`json.parse(body, List(a))`).

### Why `type` parameters come last

`type` parameters **always appear after regular data parameters**, and
are grouped contiguously when there are multiple:

```silt
-- Correct
fn parse(body: String, type a) -> Result(a, DecodeError) { ... }
fn cast(x: a, type b) -> b { ... }
fn convert(x: a, type b, type c) -> (b, c) { ... }

-- Incorrect — type param before data, won't parse
fn broken(type a, body: String) -> Result(a, DecodeError) { ... }
```

The reason is **pipe ergonomics**. Silt's `|>` operator inserts the
piped value as the first argument of the right-hand call. If `type`
parameters came first, every type-directed operation would break out
of pipelines:

```silt
let text = io.read_file(path)?
let done = text
  |> json.parse(Todo) -- works: parse(text, Todo)
  |> result.map(process)
```

`io.read_file` returns `Result(String, IoError)`, and `?` unwraps it to
the `String` that `json.parse` wants. The "type params last" convention
is what keeps the second and third arms (`json.parse(Todo)`,
`result.map(process)`) flowing in pipe form.

The rule is a single convention that keeps silt's pipe-first idiom
working cleanly across the entire type-directed surface
(`json.parse`, `toml.parse`, `decode`, `try_from`, user-defined
serialisers).

### When to use `type` parameters

Use `type a` only when a type variable genuinely cannot be anchored to
a data parameter. Most functions don't need them — if the type can be
inferred from an argument, don't add a `type` parameter just to make
the choice explicit. The existing value argument is already doing that
work.

| Function shape | Use |
|---|---|
| `map(xs: List(a), f: Fn(a) -> b) -> List(b)` | No `type` params; both vars from args |
| `parse(body: String) -> Result(a, ParseError) where a: Parse` | Error — a bound names `a`, no parameter binds it |
| `parse(body: String, type a) -> Result(a, ParseError)` | Correct form |
| `default(type a) -> a` | `a` is the only parameter |

## Inference, annotation, and ascription

Silt offers three ways to pin down a polymorphic expression's type,
from most to least preferred:

**1. Inference from context (default).** The compiler propagates types
from surrounding code — variable annotations, argument positions,
return types:

```silt
fn process(xs: List(Int)) -> Int { ... }

let result = process([1, 2, 3])   -- list type inferred as List(Int)
```

**2. Variable annotation.** When a binding needs a specific type,
annotate it:

```silt
let xs: List(Int) = []
let todo: Todo = json.parse(body, Todo)
```

**3. `as` ascription.** For expressions not bound to a variable, use
`as`:

```silt
let r = (int.parse("42") as Result(Int, int.IntError))?
let none = [] as List(Int)
```

Silt has **no turbofish** (`::<T>`), **no `fn<T>` binder syntax**, and
**no explicit type application** operator. If the compiler can't
determine a type, one of the three mechanisms above always suffices.

## Pipe interaction

`|>` inserts the left-hand value as the first argument of the call on
the right. With the "type params last" rule, type-directed functions
compose naturally:

```silt
-- `bytes.to_string` returns `Result(String, BytesError)`, while
-- `json.parse` returns `Result(_, JsonError)`. Wrap each step's error
-- in a shared enum so the chain composes through `?`:
type LoadError {
  Decode(bytes.BytesError),
  Parse(json.JsonError),
}

fn load_config(raw_bytes) -> Result(Config, LoadError) {
  let s = raw_bytes |> bytes.to_string |> result.map_err(Decode)?
  json.parse(s, Config) |> result.map_err(Parse)
}
```

For functions where the piped value should land somewhere other than
the first argument, rewrite as a lambda:

```silt
-- Instead of trying to pipe into the second slot, use a lambda:
value |> { v -> combine(a, v, c) }
```

Silt deliberately does not provide a placeholder marker (e.g. `_`) for
pipe target position. Keeping `|>` to a single rule — "insert as first
argument" — is a core simplicity bet.

## Trait interactions

### Generic functions using traits

A `where` clause lets a generic function call trait methods:

```silt
import list
import string

fn shout(items: List(a)) -> List(String) where a: Display {
  items |> list.map { x -> "{x}" |> string.to_upper }
}
```

### Generic trait implementations

Traits can be implemented on parameterized types, with optional
constraints on the parameters. See the [Traits](traits.md) guide for
the full specification:

```silt
type Box(a) {
  Box(a),
}

trait Display for Box(a) where a: Display {
  fn display(self) -> String {
    match self {
      Box(inner) -> "Box({inner})"
    }
  }
}
```

### Parameterized trait declarations

Traits can take type parameters, letting the same trait name represent
a family of related interfaces:

```silt
trait Convertible(b) {
  fn convert(self) -> b
}

trait Convertible(Int) for String {
  fn convert(self) -> Int {
    0
  }
}

-- A second impl for String at another trait argument: the argument a
-- use decides selects it.
trait Convertible(Float) for String {
  fn convert(self) -> Float {
    0.0
  }
}
```

A parameterized trait can bound its own parameters with `where`
clauses. Every impl must supply type args that satisfy the bounds:

```silt
trait HashTable(k) where k: Hash + Equal {
  fn keys(self) -> List(k)
}

trait HashTable(String) for MyStore { ... }   -- OK, String has Hash + Equal
trait HashTable(Fn(Int) -> Int) for OtherStore { ... }
-- error: type 'Fn(Int) -> Int' does not implement trait 'Hash'
```

Supertraits can also reference the enclosing trait's params. The args
flow automatically — a `where x: Child(Int)` constraint lets `x` be
given to `Parent`'s methods with Parent's own params bound to the same
`Int`:

```silt
trait Parent(a) {
  fn parent_method(self) -> a
}

trait Child(a): Parent(a) {
  fn child_method(self) -> a
}

fn use_parent(x: b, type a) -> a where b: Child(a) {
  Parent.parent_method(x) -- returns a, bound by Child's arg
}
```

`Convertible(Int)` and `Convertible(Float)` are two impls for `String`,
told apart by their whole trait argument: `let n: Int = Convertible.convert(s)`
reaches the first, `let x: Float = Convertible.convert(s)` the second. A use
whose trait argument nothing decides means no impl, and is an error. See
[Traits — Parameterized Traits](traits.md#parameterized-traits) for how
impls are told apart, and `From`.

In a `where` clause, trait arguments can be concrete types or
lowercase type variables bound elsewhere in the signature:

```silt
fn try_from(x: a, type b) -> b where a: Convertible(b) {
  Convertible.convert(x)
}
```

The compiler substitutes the trait's declared parameters with the
supplied arguments when resolving method types at the call site.

**Rules:**
- Trait declaration parameters must be lowercase type variables
  (`trait Foo(a, b)`). Binders must be distinct.
- An impl must supply exactly one argument per declared parameter;
  arity mismatch is a compile-time error.
- Parameterless traits (`trait Display { ... }`) are the common case
  and continue to work as before.

### Supertrait expansion

Constraints transitively include supertraits. Given a user-defined
`Ordered` trait whose declaration names `Equal` as a supertrait
(hypothetical user-defined trait, not built-in), a `where a: Ordered`
bound gives `a` what `Equal` gives as well:

```silt
trait Ordered: Equal {
  fn less(self, other: Self) -> Bool
}

fn sorted_unique(xs: List(a)) -> List(a) where a: Ordered {
  -- Both Ordered.less(a, b) and a == b (from the transitively-
  -- included Equal supertrait) work here.
  ...
}
```

Among the built-in traits, `Compare` includes `Equal` in the same way
(a `where a: Compare` bound lets the body use `==`); the example uses a
user-defined `Ordered` to show the supertrait edge being written.

### Traits of type constructors

A type variable written with a type argument, `m` in `xs: m(Int)`,
stands for a type constructor: `List(_)`, `Option(_)`, `Result(_, e)`.
Bounded by one of the built-in traits of type constructors, `Functor`,
`Applicative` or `Monad`, one function works over all of them:

```silt
fn double_all(xs: m(Int)) -> m(Int) where m: Functor {
  Functor.map(xs) { x -> x * 2 }
}

fn main() {
  println(double_all([1, 2])) -- [2, 4]
  println(double_all(Some(3))) -- Some(6)
}
```

See [Functors](functors.md) for constructor variables, the three traits,
their impls and the laws.

### The four structural traits

`Equal`, `Hash`, `Compare`, and `Display` are traits the language
answers itself, by the structure of a type: a type has one of them when
every type it is made of has it. Generic code that constrains on these
traits works against every such type:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash { ... }
-- Works for List(Int), List(User), List(Option(String)), ...
```

A type that holds a function has none of them (a channel has `Equal`
only, a map or a set no `Compare`): the error is at the use that needs
the trait. `Display` is the one a program may write an impl of
(`trait Display for T`); an impl of `Equal`, `Hash` or `Compare` is an
error. See [Built-in Traits](traits.md#built-in-traits).

### Trait methods on types

Some trait methods take no value of the type and only make one —
constructors like `default()` or `empty()`. A call through the trait
could not say which impl it means, so these are called on a **type**:
a bare type name (`Int.default()`), a type variable of the signature
with the bound (`a.default()`), or a `type a` parameter:

```silt
trait Default {
  fn default() -> Self
}

trait Default for Int {
  fn default() -> Self {
    0
  }
}

fn default(type a) -> a where a: Default {
  a.default() -- dispatches to Int.default at the call site
}

fn main() {
  println(default(Int)) -- 0
  println(Int.default()) -- 0
}
```

The rules:

- **The impl is the bound's.** A call of `default(Int)` passes, with the
  descriptor, the impl `Default for Int` that `where a: Default` asks
  for; `a.default()` calls its method. `default(Todo)` passes
  `Default for Todo`; etc.
- **A type variable is a type.** A type variable the signature binds
  (or the impl's header, in its methods) is a type in the body, where no
  value is named so: `fn concat_all(xs: List(a)) -> a where a: Monoid`
  calls `a.empty()` with no `type a` parameter, and `trait Default for
  Box(a) where a: Default` makes `Box(a.default())`. A `type a`
  parameter stays the way to pass a type to a call (`default(Int)`,
  `json.parse(body, Todo)`). A value named `a` (a parameter `a: List(a)`)
  is the value: `a.empty()` is then a field of it, and the error says to
  rename one.
- **The descriptor is a dispatch key, not an argument.** It occupies no
  parameter of the method: `a.empty()` passes nothing.
- **A method that takes a value of the type is called through its
  trait.** `Monoid.combine(x, y)` reaches the impl for the type of its
  arguments; `a.combine(x, y)` and `Int.combine(1, 2)` are errors whose
  help names that call.
- **`where` constraints are required** for the generic path. Writing
  `fn f(type a) -> a { a.default() }` without `where a: Default`
  rejects at the call to `a.default()` — the compiler can't prove an
  impl exists.
- **A method that makes a value is called on a type, never on a value
  or through its trait.** `x.default()` on a value `x: a` is an error (a
  dot after a value names a field) whose help names the call to write:
  `a.default()`. `Default.default()` is an
  error that says to call it on a type.
- **Ambiguity across traits is rejected.** If both `Foo` and `Bar`
  declare a method `build` and `a` is constrained to `Foo + Bar`,
  calling `a.build()` errors with "ambiguous method 'build' on `type a`:
  provided by traits Bar, Foo", and the help says to call it from a
  function bounded by only one of them.

This is how `default`, `empty`, and similar constructor-style trait
methods become directly writable in user code — without silt growing
`T::method()` path syntax or inherent impls.

### Associated types

A trait can declare a **type member** alongside its methods. Each impl
binds the member to a concrete type, and the trait's methods may refer
to that member through `Self::Item` projection. This lets a single
trait relate one self-type to a family of related types without paying
for a trait parameter at every use site.

Declarations live on the trait; bindings live on each impl:

```silt
trait Stream {
  type Item
  fn first(self) -> Self::Item
}

type Wrap {
  v: Int,
}

trait Stream for Wrap {
  type Item = Int

  fn first(self) -> Int {
    self.v
  }
}
```

Inside the trait body, `Self::Item` refers to whatever the impl will
bind. At the call site (`Stream.first(w)` on a `Wrap`), the compiler reduces
the projection to `Int` via the impl's `type Item = Int` binding.

A trait may declare more than one associated type:

```silt
trait Pair {
  type First
  type Second
  fn first(self) -> Self::First
  fn second(self) -> Self::Second
}

type IntStringPair {
  a: Int,
  b: String,
}

trait Pair for IntStringPair {
  type First = Int
  type Second = String

  fn first(self) -> Int {
    self.a
  }
  fn second(self) -> String {
    self.b
  }
}
```

#### Bounds on associated types

A declaration may attach trait bounds to the member. Bindings in impls
must satisfy those bounds:

```silt
trait Container {
  type Item: Compare
  fn first(self) -> Self::Item
}

type Box {
  v: Int,
}

trait Container for Box {
  type Item = Int -- OK: Int has Compare
  fn first(self) -> Int {
    self.v
  }
}
```

Multiple bounds combine with `+`, the same way `where` bounds
do: `type Item: Compare + Hash`.

#### Qualified projection: `<T as Trait>::Item`

Inside a trait body or impl, `Self::Item` is the natural form. Outside
a trait — in a free function's signature, for example — write the
fully-qualified `<T as Trait>::Item` form to project the associated
type of a known concrete impl:

```silt
fn first_out(w: Wrap) -> <Wrap as Producer>::Out {
  w.produce()
}
```

The compiler resolves the projection through the registered impl, so
the return type reduces to whatever `type Out = ...` the impl bound.

#### Supertrait inheritance

A subtrait inherits its supertrait's associated types automatically.
You bind them once on the supertrait impl and reference them from
either trait body:

```silt
trait Super {
  type Item
  fn one(self) -> Self::Item
}

trait Sub: Super {
  fn first(self) -> Self::Item -- Self::Item comes from Super
}

type Wrap {
  v: Int,
}

trait Super for Wrap {
  type Item = Int
  fn one(self) -> Int {
    self.v
  }
}

trait Sub for Wrap {
  fn first(self) -> Int {
    self.v + 1
  }
}
```

#### Rules

- **Declarations live on the trait.** `type Item` (optionally
  `type Item: Compare + Hash`) appears inside the trait body alongside
  method declarations. Order is free — declarations and methods may
  interleave.
- **Bindings live on each impl.** Every concrete impl must supply
  `type Item = ConcreteType` for every associated type the trait
  declares. Missing bindings are a compile-time error
  (`missing required associated type 'Item'`).
- **No defaults in v1.** `type Item = SomeDefault` on a trait
  declaration is rejected — silt reserves the syntax for a future
  extension. Bindings are required on every impl.
- **No duplicates.** Two `type Item = ...` bindings in the same impl
  are a compile-time error.
- **`Self::Item` is trait-local.** Outside any trait body, `Self`
  alone is meaningless; use `<T as Trait>::Item` to project from a
  named type.

## Worked examples

### Collection operations

```silt
fn map(xs: List(a), f: Fn(a) -> b) -> List(b) { ... }

fn filter(xs: List(a), f: Fn(a) -> Bool) -> List(a) { ... }

fn fold(xs: List(a), init: b, f: Fn(b, a) -> b) -> b { ... }

fn group_by(xs: List(a), key: Fn(a) -> k) -> Map(k, List(a)) where k: Hash + Equal { ... }
```

### Option and Result combinators

```silt
fn map(opt: Option(a), f: Fn(a) -> b) -> Option(b) { ... }

fn and_then(opt: Option(a), f: Fn(a) -> Option(b)) -> Option(b) { ... }

fn map(r: Result(a, e), f: Fn(a) -> b) -> Result(b, e) { ... }

fn map_err(r: Result(a, e), f: Fn(e) -> f) -> Result(a, f) { ... }
```

### Type-directed decoding

```silt
fn parse(body: String, type a) -> Result(a, DecodeError) where a: Parse { ... }
fn from_toml(content: String, type a) -> Result(a, DecodeError) where a: Parse { ... }

-- call sites
let config = from_toml(raw, AppConfig)?
let todo = body |> json.parse(Todo)
```

The stdlib follows the same shape: `json.parse(src, T)` and `toml.parse(src, T)`
are the built-in type-directed decoders — hover either name in your editor
for the full per-call documentation surfaced by the LSP.

### Conversion

silt does not ship a built-in `Into` / `TryInto` trait. Define your
own conversion trait and implement it per source/target pair:

```silt
trait Convert(b) {
  fn convert(self) -> b
}

trait Convert(Int) for String {
  fn convert(self) -> Int {
    0
  }
}

fn into(x: a, type b) -> b where a: Convert(b) {
  Convert.convert(x)
}
```

The same pattern adapts to fallible conversions by changing the trait
method's return type to `Result(b, e)` for whatever error type the
caller wants. A type has one impl of the trait for each trait
argument, so `Convert(Int) for String` and `Convert(Float) for String`
are two impls, and the use's trait argument says which one it reaches:
`into("7", Int)` the first, `into("7", Float)` the second (see the
"Parameterized trait declarations" subsection above).

### User-defined generic container

```silt
import map

type Cache(k, v) {
  store: Map(k, v),
  capacity: Int,
}

fn get(c: Cache(k, v), key: k) -> Option(v) where k: Hash + Equal {
  map.get(c.store, key)
}

fn put(c: Cache(k, v), key: k, value: v) -> Cache(k, v) where k: Hash + Equal {
  c.{ store: map.set(c.store, key, value) }
}
```

## What silt deliberately does not have

Each exclusion is a design choice, not an oversight. The rationale is
preserving silt's minimalism and keeping the mental model small.

### No explicit generic binders (`fn<T>`)

Type variables are introduced by first use in a parameter annotation.
Declaring them separately at the top of a function would duplicate
information the compiler already has and add punctuation silt
otherwise avoids.

### No turbofish or explicit type application

When the compiler can't infer a type, annotation or `as` ascription
always works. Turbofish would be a fourth mechanism serving the same
purpose.

### No constants in type parameters

No `Array(n, Int)` where `n` is a value. Fixed-size arrays don't exist
in silt; `List` is used throughout. If bounded-size containers become
necessary, they'll be added as specific types, not as a generic
feature.

### No existential return types or trait objects

No `fn make() -> some Parse` or `dyn Parse`. Patterns that would use
these in other languages are expressed with enums (a closed set of
concrete types) or with concrete wrapper types.

### No higher-rank polymorphism

A function cannot require a polymorphic function as an argument
(`fn apply(f: (forall a. a -> a), ...)`). Hindley–Milner is
rank-1; silt doesn't extend it.

### No user-definable macros or compile-time code

No `macro_rules!`, no `comptime`, no quasi-quotation. Type-directed
runtime behaviour (like `json.parse`) covers the common derive-shaped
use cases without introducing a second language stage.

## Summary of rules

1. Lowercase identifiers in type positions are type variables. One
   written with a type argument (`m(Int)`) is a type constructor
   variable: it stands for a constructor (`Option(_)`), takes one type
   argument, and is bounded by a trait of type constructors (`where m:
   Functor`).
2. Uppercase identifiers are concrete types or type constructors.
3. Type variables bind at first appearance in a parameter type.
4. Every type variable must be anchored — appear in a regular
   parameter's type, or be declared as a `type a` parameter.
   Inside the function it stands for any type: the body cannot decide
   it, and may use only what the `where` clauses promise of it.
5. `type` parameters always come after data parameters, grouped
   contiguously.
6. Constraints go in `where` clauses. Multiple bounds on one variable
   use `+`; bounds on multiple variables use `,`.
7. Call sites never declare type arguments explicitly — the compiler
   infers them. Annotation, ascription, or a `type` parameter covers
   the rare cases when inference can't decide.
