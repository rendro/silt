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
   Uppercase identifiers are concrete types or type constructors.
2. **Type variables bind at their first appearance** in a signature,
   reading left to right.
3. **Every type variable must be anchored to a parameter.** Either it
   appears inside the type of a regular parameter, or it is introduced
   explicitly as a `type a` parameter (see [Return-polymorphic functions](#return-polymorphic-functions)).
4. **The compiler does the rest.** Polymorphism, specialisation,
   constraint resolution, and instantiation at call sites are all
   inferred. There is no `fn<T>` binder syntax and no turbofish.

```silt
fn map(xs: List(a), f: Fn(a) -> b) -> List(b)
--             ↑              ↑         ↑
--        a binds here   b binds here   both already in scope
```

## Generic type declarations

Type parameters are declared in parentheses after the type name:

```silt
type Option(a) { Some(a), None }
type Result(a, e) { Ok(a), Err(e) }
type Pair(a, b) { first: a, second: b }
type Tree(a) { Leaf, Node(Tree(a), a, Tree(a)) }
```

Parameters are lowercase. They are in scope throughout the declaration
body, including in constructor argument types, record fields, and
recursive self-references.

At use sites, types are applied positionally: `Option(Int)`,
`Result(String, Error)`, `Pair(User, List(Role))`.

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
fn identity(x: a) -> a { x }

fn swap(pair: (a, b)) -> (b, a) {
  let (x, y) = pair
  (y, x)
}

fn compose(f: Fn(b) -> c, g: Fn(a) -> b) -> Fn(a) -> c {
  fn(x) { f(g(x)) }
}
```

Every lowercase name in a type annotation that is not already bound
becomes a fresh type variable at the binding point. Subsequent uses of
the same name refer to the same variable.

Type variables may appear:

- As a full parameter type: `x: a`
- Inside a type constructor: `xs: List(a)`, `m: Map(k, v)`
- Inside a function type: `f: Fn(a) -> b`
- In the return type, **only if already bound** by a parameter

### The binding rule

Every type variable used in a signature must appear in at least one
**input position** — either a regular parameter's type, or as a `type a`
parameter (see below). A type variable that appears only in the return
type or only in a `where` clause is a compile-time error:

```silt
-- ERROR: 'a' only appears in the return type
fn make() -> a { ... }

-- ERROR: 'a' only appears in a where clause
fn parse(s: String) -> Int where a: Parse { ... }
```

This rule eliminates the "where did `a` come from?" confusion that
plagues ML-family languages with implicit `forall`. When the reader
encounters a type variable, its origin is always a parameter they can
point to.

## Where clauses

Constrain a type variable to types implementing a trait with `where`:

```silt
fn sort(xs: List(a)) -> List(a) where a: Ord {
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
  where k: Hash + Equal, v: Clone
{ ... }
```

The two forms compose freely — `where a: Equal + Hash` and
`where a: Equal, a: Hash` mean the same thing.

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

fn parse(body: String, type a) -> Result(a, Error) where a: Decode {
  a.decode(body)
}

fn try_from(x: a, type b) -> b where a: Convertible(b) {
  x.convert()
}
```

Inside the body, the `type a` parameter doubles as a dispatch target for
trait methods. `a.default()` invokes the `default` method of whichever
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

The type passed must be a concrete type (or a type expression in terms
of types already in scope). Passing a lowercase type variable only works
when that variable is in scope at the call site.

### Why `type` parameters come last

`type` parameters **always appear after regular data parameters**, and
are grouped contiguously when there are multiple:

```silt
-- Correct
fn parse(body: String, type a) -> Result(a, Error)
fn cast(x: a, type b) -> b
fn convert(x: a, type b, type c) -> (b, c)

-- Incorrect — type param before data, won't parse
fn broken(type a, body: String) -> Result(a, Error)
```

The reason is **pipe ergonomics**. Silt's `|>` operator inserts the
piped value as the first argument of the right-hand call. If `type`
parameters came first, every type-directed operation would break out
of pipelines:

```silt
http.get(url)?
|> json.parse(Todo)           -- works: parse(body, Todo)
|> result.map_ok(process)
```

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
| `parse(body: String) -> Result(a, Error)` | Error — `a` unbound |
| `parse(body: String, type a) -> Result(a, Error)` | Correct form |
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
let r = (int.parse("42") as Result(Int, String))?
[] as List(Int)
```

Silt has **no turbofish** (`::<T>`), **no `fn<T>` binder syntax**, and
**no explicit type application** operator. If the compiler can't
determine a type, one of the three mechanisms above always suffices.

## Pipe interaction

`|>` inserts the left-hand value as the first argument of the call on
the right. With the "type params last" rule, type-directed functions
compose naturally:

```silt
raw_bytes
|> string.from_utf8
|> result.map_ok(fn(s) { json.parse(s, Config) })
|> result.flatten
```

For functions where the piped value should land somewhere other than
the first argument, rewrite as a lambda:

```silt
-- Instead of trying to pipe into the second slot, use a lambda:
value |> fn(v) { combine(a, v, c) }
```

Silt deliberately does not provide a placeholder marker (e.g. `_`) for
pipe target position. Keeping `|>` to a single rule — "insert as first
argument" — is a core simplicity bet.

## Trait interactions

### Generic functions using traits

A `where` clause lets a generic function call trait methods:

```silt
fn shout(items: List(a)) -> List(String) where a: Display {
  items |> list.map { x -> x.display() |> string.upper }
}
```

### Generic trait implementations

Traits can be implemented on parameterized types, with optional
constraints on the parameters. See the [Traits](traits.md) guide for
the full specification:

```silt
type Box(T) { Box(T) }

trait Display for Box(a) where a: Display {
  fn display(self) -> String {
    match self {
      Box(inner) -> "Box({inner.display()})"
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
  fn convert(self) -> Int { ... }
}

trait Convertible(Float) for String {
  fn convert(self) -> Float { ... }
}
```

A parameterized trait can bound its own parameters with `where`
clauses. Every impl must supply type args that satisfy the bounds:

```silt
trait HashTable(k) where k: Hash + Equal {
  fn keys(self) -> List(k)
}

trait HashTable(String) for MyStore { ... }   -- OK, String auto-derives Hash + Equal
trait HashTable(Function) for OtherStore { ... }  -- error: Function does not implement Hash
```

Supertraits can also reference the enclosing trait's params. The args
flow automatically — a `where x: Child(Int)` constraint makes
`Parent`'s methods callable on `x` with Parent's own params bound to
the same `Int`:

```silt
trait Parent(a) {
  fn parent_method(self) -> a
}

trait Child(a): Parent(a) {
  fn child_method(self) -> a
}

fn use_parent(x: b, type a) -> a where b: Child(a) {
  x.parent_method()     -- returns a, bound by Child's arg
}
```

`Convertible(Int)` and `Convertible(Float)` are distinct impls — the
same source type (`String`) can implement the trait multiple times
with different target arguments.

In a `where` clause, trait arguments can be concrete types or
lowercase type variables bound elsewhere in the signature:

```silt
fn try_from(x: a, type b) -> b where a: Convertible(b) {
  x.convert()
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

Constraints transitively include supertraits. A `where a: Ordered`
bound makes methods from `Equal` (the supertrait of `Ordered`)
callable on `a` as well:

```silt
fn sorted_unique(xs: List(a)) -> List(a) where a: Ordered {
  -- Both a.less(b) and a.equal(b) are callable here
  ...
}
```

### The four auto-derived traits

`Equal`, `Hash`, `Compare`, and `Display` are auto-derived for every
user-defined type. Generic code that constrains on these traits works
against every type by default:

```silt
fn dedup(xs: List(a)) -> List(a) where a: Equal + Hash { ... }
-- Works for List(Int), List(User), List(Option(String)), ...
```

To customise, write an explicit impl — it overrides the derived one.

### Trait methods on types

Some trait methods take no `self` and only return `Self` — constructors
like `Default::default()` or `Monoid::empty()`. To invoke these without
an instance, call them on a **type descriptor**: either a bare type
name (`Int.default()`) or a `type a` parameter (`a.default()`):

```silt
trait Default {
  fn default() -> Self
}

trait Default for Int {
  fn default() -> Self { 0 }
}

fn default(type a) -> a where a: Default {
  a.default()                -- dispatches to Int.default at the call site
}

fn main() {
  let n = default(Int)       -- 0 — generic path
  let m = Int.default()      -- 0 — concrete path
}
```

The rules:

- **Dispatch is by the descriptor's carried type name.** At runtime the
  descriptor `Int` resolves to the `Default` impl for `Int`; `Todo`
  resolves to `Default for Todo`; etc.
- **The descriptor is a dispatch key, not an argument.** It doesn't
  occupy a `self` slot. Method signatures that declare `Self` as a
  parameter (e.g. `fn combine(a: Self, b: Self)`) receive only the
  caller's explicit arguments.
- **`where` constraints are required** for the generic path. Writing
  `fn f(type a) -> a { a.default() }` without `where a: Default`
  rejects at the call to `a.default()` — the compiler can't prove an
  impl exists.
- **Ambiguity across traits is rejected.** If both `Foo` and `Bar`
  declare a method `build` and `a` is constrained to `Foo + Bar`,
  calling `a.build()` errors with "ambiguous method 'build' on `type a`:
  provided by multiple traits (Foo, Bar)".

This is how `default`, `empty`, and similar constructor-style trait
methods become directly writable in user code — without silt growing
`T::method()` path syntax or inherent impls.

## Worked examples

### Collection operations

```silt
fn map(xs: List(a), f: Fn(a) -> b) -> List(b)
fn filter(xs: List(a), f: Fn(a) -> Bool) -> List(a)
fn fold(xs: List(a), init: b, f: Fn(b, a) -> b) -> b
fn group_by(xs: List(a), key: Fn(a) -> k) -> Map(k, List(a))
  where k: Hash + Equal
```

### Option and Result combinators

```silt
fn map(opt: Option(a), f: Fn(a) -> b) -> Option(b)
fn and_then(opt: Option(a), f: Fn(a) -> Option(b)) -> Option(b)
fn map_ok(r: Result(a, e), f: Fn(a) -> b) -> Result(b, e)
fn map_err(r: Result(a, e), f: Fn(e) -> f) -> Result(a, f)
```

### Type-directed decoding

```silt
fn parse(body: String, type a) -> Result(a, Error) where a: Decode
fn from_toml(content: String, type a) -> Result(a, Error) where a: Decode

-- call sites
let config = toml.from_toml(raw, AppConfig)?
body |> json.parse(Todo)
```

### Conversion

```silt
fn into(x: a, type b) -> b where a: Into(b)
fn try_from(x: a, type b) -> Result(b, Error) where a: TryInto(b)

let n: Int = into(small, Int)
let result = try_from("42", Int)
```

### User-defined generic container

```silt
type Cache(k, v) {
  store: Map(k, v),
  capacity: Int,
}

fn get(c: Cache(k, v), key: k) -> Option(v) where k: Hash + Equal {
  map.get(c.store, key)
}

fn put(c: Cache(k, v), key: k, value: v) -> Cache(k, v)
  where k: Hash + Equal
{
  c.{ store: map.insert(c.store, key, value) }
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

### No higher-kinded types

No `Functor f` abstracting over type constructors. HKT pays off
heavily for library authors writing universal abstractions but makes
type errors and inference substantially harder for everyone else.
Silt's position: the stdlib provides the common shapes directly
(`List`, `Option`, `Result`, `Map`), and that covers the overwhelming
majority of real code.

### No associated types

Traits cannot declare `type Item` alongside their methods. If a trait
needs to relate multiple types, it takes them as trait parameters:

```silt
trait Into(b) { fn into(self) -> b }
```

This costs a parameter at every use site but avoids the complexity of
projection types (`<T as Trait>::Item`) and family-dependent inference.

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

1. Lowercase identifiers in type positions are type variables.
2. Uppercase identifiers are concrete types or type constructors.
3. Type variables bind at first appearance in a parameter type.
4. Every type variable must be anchored — appear in a regular
   parameter's type, or be declared as a `type a` parameter.
5. `type` parameters always come after data parameters, grouped
   contiguously.
6. Constraints go in `where` clauses. Multiple bounds on one variable
   use `+`; bounds on multiple variables use `,`.
7. Call sites never declare type arguments explicitly — the compiler
   infers them. Annotation, ascription, or a `type` parameter covers
   the rare cases when inference can't decide.
