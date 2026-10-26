---
title: "Functors"
section: "Language"
order: 9
---

# Functors

Most traits are about a type: `Display` is what `Int` or `Shape` has.
`Functor`, `Applicative` and `Monad` are about a **type constructor**:
`List`, `Option`, `Result(_, e)`, a type that still needs one type
argument. With them one function works over every such type:

```silt
fn double_all(xs: m(Int)) -> m(Int) where m: Functor {
  Functor.map(xs) { x -> x * 2 }
}

fn main() {
  println(double_all([1, 2, 3])) -- [2, 4, 6]
  println(double_all(Some(4))) -- Some(8)
  let r: Result(Int, String) = Ok(5)
  println(double_all(r)) -- Ok(10)
}
```

This page is about generic code over type constructors. Code that knows
its type keeps its module's functions: `list.map`, `option.map`,
`result.flat_map`.

## Type constructors

A type with parameters, given all but its first type argument, is a
type constructor. It is written with `_` where the first argument goes:

| Type constructor | Applied to `Int` |
|------------------|------------------|
| `List(_)` | `List(Int)` |
| `Option(_)` | `Option(Int)` |
| `Result(_, String)` | `Result(Int, String)` |
| `Map(_, Bool)` | `Map(Int, Bool)` |
| `Tree(_)`, a program's `type Tree(a)` | `Tree(Int)` |

A type constructor always stands for the **first** type argument of its
type. `Result(_, e)` maps over the `Ok` value with the error fixed; there
is no constructor that maps over the error (that is `result.map_err`).
A `Map`'s first argument is its key, so a map is no Functor over its
values. A type without parameters (`Int`, `Shape`), a function type, a
tuple and a record are no type constructors.

## Constructor variables

A lowercase name written with a type argument in a signature is a
**constructor variable**: `m` in `xs: m(Int)`. A call decides it as the
constructor of the type it is given, as it decides `a` in `x: a`:

```silt
fn keep(xs: m(a)) -> m(a) {
  xs
}

fn swap(p: (m(a), m(b))) -> (m(b), m(a)) {
  match p {
    (x, y) -> (y, x)
  }
}

fn main() {
  println(keep([1, 2])) -- [1, 2]
  println(swap((Some(1), Some("a")))) -- (Some(a), Some(1))
}
```

`m(Int)` matches `List(Int)`, `Option(Int)`, `Result(Int, e)` (then `m`
is `Result(_, e)`) and a `Tree(Int)`; it does not match `Int`, a
function, a tuple or a record. In one call `m` is one constructor:
`swap((Some(1), [2]))` is an error.

The rules:

- A constructor variable takes exactly one type argument: `m(a, b)` is
  an error.
- A variable is a type or a constructor in the whole signature, not
  both: `fn f(xs: m(Int), y: m)` is an error.
- The binding rule holds as for any type variable: `m` appears in a
  parameter's type, or is a `type m` parameter (see [Passing a
  constructor](#passing-a-constructor)).
- A declared type's parameters are types: `type Wrapped(m, a) {
  W(m(a)) }` is an error. Constructor variables appear in function
  signatures, in trait methods as `Self(a)`, and as `type m`
  parameters.
- A bound is on the variable (`where m: Functor`), never on an applied
  one: generic code cannot show or compare an `m(Int)`. It returns it,
  and the caller, which knows the type, shows it.

Inside the function `m` is any constructor: the body cannot take an
`m(Int)` for a `List(Int)` or match it against `Some`.

## Functor, Applicative and Monad

The three traits are built in, named without an import, like `Compare`.
Their declarations:

```
trait Functor {
  fn map(xs: Self(a), f: Fn(a) -> b) -> Self(b)
}

trait Applicative: Functor {
  fn pure(x: a) -> Self(a)
  fn map2(xs: Self(a), ys: Self(b), f: Fn(a, b) -> c) -> Self(c)
}

trait Monad: Applicative {
  fn flat_map(xs: Self(a), f: Fn(a) -> Self(b)) -> Self(b)
}
```

A method that takes a value of the type is called through its trait,
data first and the function last, so pipes and trailing closures work
as with `list.map`. `pure`, which only makes a value, is called on a
type, as every method that makes one is (see [Methods That Make a
Value](traits.md#methods-that-make-a-value)):

```silt
import string

fn main() {
  println(Functor.map([1, 2]) { x -> x + 1 }) -- [2, 3]
  println(Some(2) |> Functor.map { x -> x * 10 }) -- Some(20)
  println(Applicative.map2(Some(1), Some(2)) { a, b -> a + b }) -- Some(3)
  println(Monad.flat_map(["a b", "c"]) { s -> string.split(s, " ") }) -- [a, b, c]
  println(Option.pure(1)) -- Some(1)
}
```

At a known type, write the module's function and the constructor:
`list.map(xs, f)`, not `Functor.map(xs, f)`; `Some(1)`, not
`Option.pure(1)`. Both are accepted, and the built-in impls are those
functions (the same results, the same messages), but the docs and the
examples write the module's. The traits are for generic code.

## The built-in impls

| Type | Functor `map` | Applicative `pure`, `map2` | Monad `flat_map` |
|------|---------------|----------------------------|------------------|
| `List(_)` | `list.map` | `[x]`; every pair | `list.flat_map` |
| `Option(_)` | `option.map` | `Some(x)`; the first `None` | `option.flat_map` |
| `Result(_, e)` | `result.map` | `Ok(x)`; the first `Err` | `result.flat_map` |
| `Channel(_)` | `stream.map` | — | — |

`map2` of two lists gives every pair, for each element of the first each
element of the second, in that order; pairwise is `list.zip` and then
`list.map`. Of two options or results it gives the first `None` or
`Err`, the left one first:

```silt
fn main() {
  println(Applicative.map2([1, 2], [10, 20]) { a, b -> a + b }) -- [11, 21, 12, 22]
  let bad: Result(Int, String) = Err("left")
  let worse: Result(Int, String) = Err("right")
  println(Applicative.map2(bad, worse) { a, b -> a + b }) -- Err(left)
}
```

A channel is a Functor only: its `map` is `stream.map`, which starts a
stage task (see [stream](../stdlib/stream.md)), so generic code over `m`
starts one when `m` is `Channel(_)`. `Set` has no Functor (`set.map`
needs `Hash` of its results), and neither do `Map` and functions.

These impls are built in: no module may write an impl of one of the
three traits for a built-in type, and a program's `trait Monad` is an
error, as a `trait Display` is.

## Generic code

A function bounded by one of the traits works over every type with an
impl, and reaches the supertraits' methods through its bound (`m:
Monad` gives `map` and `pure` too). `m.pure(x)` makes a value of the
caller's constructor:

```silt
fn pair_up(xs: m(a), ys: m(b)) -> m((a, b)) where m: Applicative {
  Applicative.map2(xs, ys) { a, b -> (a, b) }
}

fn step(xs: m(Int)) -> m(Int) where m: Monad {
  Monad.flat_map(xs) { x -> m.pure(x + 1) }
}

fn main() {
  println(pair_up(Some(1), Some("a"))) -- Some((1, a))
  println(pair_up([1, 2], ["x"])) -- [(1, x), (2, x)]
  println(step(Some(1))) -- Some(2)
  println(step([1, 10])) -- [2, 11]
}
```

A function without annotations is generic over the constructor too, its
bound inferred, as a bound on a type is:

```silt
fn twice(xs) {
  Functor.map(xs) { x -> x * 2 }
}

fn main() {
  println(twice([1, 2])) -- [2, 4]
  println(twice(Some(3))) -- Some(6)
}
```

Hover in an editor shows its signature, `fn twice(xs: m(Int)) -> m(Int)
where m: Functor`, and `:type twice` in the REPL `Fn(m(Int)) -> m(Int)
where m: Functor`.

### Passing a constructor

A constructor that appears only in the result is named at the call with
a `type m` parameter. A type given to it is that type's constructor:
`Option` is `Option(_)`, `Result` is `Result(_, e)` with `e` the
caller's:

```silt
fn wrap(x: a, type m) -> m(a) where m: Applicative {
  m.pure(x)
}

fn main() {
  println(wrap(1, Option)) -- Some(1)
  println(wrap(1, List)) -- [1]
  let r: Result(Int, String) = wrap(1, Result)
  println(r) -- Ok(1)
}
```

`wrap(1, Int)` is an error: `'Int' is a type, but 'm' is a type
constructor`.

## traverse and sequence

`list.traverse` calls a function that gives a value of an Applicative
on every element, and gives one value of it that holds the list of the
results: the first `None` or `Err`, if there is one. `list.sequence`
does the same with a list of such values:

```silt
import int
import list

fn main() {
  println(["1", "2"] |> list.traverse(int.parse)) -- Ok([1, 2])
  println(["1", "x"] |> list.traverse(int.parse)) -- Err(invalid digit at byte 0)
  println(list.sequence([Some(1), None])) -- None
  println(list.traverse([1, 2]) { x -> [x, -x] }) -- [[1, 2], [1, -2], [-1, 2], [-1, -2]]
}
```

Of lists they give every combination. They work over any Applicative,
your own types' too.

`list.traverse` calls `f` on every element, left to right, also after
one has failed, and then gives the first failure. For a function
without effects (`int.parse`) only the time differs. Where the steps
must stop at the first failure (reading files, sending requests), write
the loop with `?`:

```silt
import io
import list
import result

fn read_all(paths: List(String)) -> Result(List(String), io.IoError) {
  match paths {
    [] -> Ok([])
    [path, ..rest] -> {
      let text = io.read_file(path)?
      let others = read_all(rest)?
      Ok(list.prepend(others, text))
    }
  }
}

fn main() {
  println(read_all([])) -- Ok([])
  println(read_all(["/no/such/file", "/etc/hostname"]) |> result.is_err) -- true
}
```

## An impl for your own type

An impl of a trait of type constructors writes `_` for the first type
argument of its type, the one the trait maps over. Its methods write
their types with the type (`Tree(a)`), with `Self(a)`, or not at all:

```silt
type Tree(a) {
  Leaf,
  Node(Tree(a), a, Tree(a)),
}

trait Functor for Tree(_) {
  fn map(t, f) {
    match t {
      Leaf -> Leaf
      Node(l, x, r) -> Node(Functor.map(l, f), f(x), Functor.map(r, f))
    }
  }
}

fn double_all(xs: m(Int)) -> m(Int) where m: Functor {
  Functor.map(xs) { x -> x * 2 }
}

fn main() {
  println(double_all(Node(Leaf, 1, Node(Leaf, 2, Leaf)))) -- Node(Leaf, 2, Node(Leaf, 4, Leaf))
}
```

The header's rules:

- Exactly one `_`, as the first argument: `trait Functor for Option(_)`,
  `trait Functor for Result(_, e)`. Without one it is an error, whose
  help writes the header; `_` elsewhere (`Pair(a, _)`) is an error too:
  declare the argument to map over first in the type.
- The other arguments are distinct lowercase names, which a `where` on
  the impl may bound: `trait Functor for Tagged(_, t) where t: Display`.
  What the header asks is owed wherever generic code reaches the impl.
- A type without parameters has no such impl: `trait Functor for
  Shape(_)` is "type 'Shape' takes no type arguments, so it is not a
  type constructor".
- `_` in an impl of a trait of types (`trait Show for Box(_)`, with a
  `Show` whose methods write `Self` bare) is an error: name every type
  argument.
- The impl is written in the module that declares the type, as any impl
  of a built-in trait.

An impl writes all of its trait's methods: a default body cannot give a
supertrait's method (`map` from `pure` and `map2`). An `Applicative`
impl needs a `Functor` impl for the type, and a `Monad` impl an
`Applicative` one.

## Traits of type constructors of your own

A trait whose methods write `Self` with a type argument is a trait of
type constructors; a trait that writes `Self` in neither form is one when
its supertraits are:

```silt
import list

trait Container {
  fn size(xs: Self(a)) -> Int
}

trait Container for List(_) {
  fn size(xs) {
    list.length(xs)
  }
}

fn total(xs: m(a), ys: m(b)) -> Int where m: Container {
  Container.size(xs) + Container.size(ys)
}

fn main() {
  println(total([1, 2], ["a"])) -- 3
}
```

Such a trait names its parameters with their types (`xs: Self(a)`): a
`self` in it is an error. It may declare methods, default bodies, method
`where` clauses and supertraits that are themselves traits of type
constructors, and not:

- `Self` both with and without a type argument (a trait is for types or
  for type constructors, not both);
- a supertrait of types (`trait Sized: Display`);
- trait parameters (`trait MonadError(e)`) or associated types.

## The laws

Functor, Applicative and Monad come with laws that every impl should
keep. No checker can prove them; the built-in impls are tested against
them, and your own impls are on their honour. With `map` for
`Functor.map`, `map2` for `Applicative.map2`, `flat_map` for
`Monad.flat_map`, and `m` the impl's constructor:

```
map(xs, { x -> x }) == xs
map(map(xs, f), g) == map(xs, { x -> g(f(x)) })
map2(m.pure(x), ys, f) == map(ys, { y -> f(x, y) })
map2(xs, m.pure(y), f) == map(xs, { x -> f(x, y) })
flat_map(m.pure(x), f) == f(x)
flat_map(xs, m.pure) == xs
flat_map(flat_map(xs, f), g) == flat_map(xs, { x -> flat_map(f(x), g) })
map2(xs, ys, f) == flat_map(xs, { x -> map(ys, { y -> f(x, y) }) })
```

The last one is why `map2` of two lists gives every pair: it agrees
with `flat_map`.

## Messages

The kind errors have the code `E0343` (see [error
codes](error-codes.md)):

```
error[type]: 'm' is written with a type argument in `xs: m(Int)` and without one in `y: m`: a type variable is a type or a type constructor, not both
error[type]: 'a' is a type, but 'Functor' is a trait of type constructors
  = help: write the variable with its type argument, as in `xs: a(Int)`
error[type]: trait 'Functor' is for type constructors: its impl writes '_' for the first type argument
  = help: write `trait Functor for Option(_)`
```

A call with a type that no constructor fits says what `m` matches:

```
error[type]: type mismatch: expected m(Int), got Int
  = note: 'm' stands for a type constructor: it matches a type with a type argument, like 'List(Int)' or 'Option(Int)'
error[type]: type 'Set(_)' does not implement trait 'Functor'
```
