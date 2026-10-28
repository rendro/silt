---
title: "Language card"
section: "Guide"
order: 6
description: "All of silt on one page: the syntax in brief, then the signature of every builtin function, constant, type and trait."
---

# Language card

silt on one page: the syntax in brief, then every function, constant,
type and trait of the standard library. The [Language Guide](language-guide.md)
and the reference pages of the standard library say more about each.

## A program

```silt
import list

type Shape {
  Circle(Int),
  Rect(Int, Int),
}

fn area(s: Shape) -> Int {
  match s {
    Circle(r) -> 3 * r * r
    Rect(w, h) -> w * h
  }
}

fn main() {
  let shapes = [Circle(1), Rect(2, 3)]
  let total = shapes |> list.map { s -> area(s) } |> list.sum
  println("total area: {total}") -- total area: 9
}
```

A file is a module. `silt run main.silt` runs its `fn main()`; a `main`
that returns `Err(e)` ends the program with status 1. `silt run main.silt
-- a b` gives the program its arguments (`io.args()`).

## Lexical

- Comments: `-- to the end of the line`, `{- a block, {- nested -} -}`.
- Keywords: `as else fn import let loop match opaque pub return trait type
  when where`. `true` and `false` are literals.
- Names: values and functions `snake_case`; types, traits and variants
  `CamelCase`; a type variable is a lowercase name (`a`, `e`, `item`).
- Each statement and declaration starts on a line of its own.
  Indentation never changes meaning. A statement goes on in the next
  line after an open bracket or an operator that ends the line, and
  before a line that starts with an infix operator other than `+` and
  `-` (`|> list.map { ... }`). A call, `?` and a trailing closure stay on
  the line of what they follow.

## Values

| Kind | Literal |
|------|---------|
| `Int`, 64-bit, overflow is a runtime error | `42`, `-7`, `1_000`, `0xFF`, `0b1010` |
| `Float`, always finite | `3.5`, `1e3`, `2.5e-3` |
| `Bool` | `true`, `false` |
| `String`, UTF-8 | `"hi {name}, {n + 1}"`, escapes `\n \t \\ \" \{ \} \u{e9}` |
| raw string, no escapes or interpolation | `"""C:\dir {x}"""` |
| unit | `()` |
| tuple | `(1, "a")`, one element `(1,)` |
| list | `[1, 2, 3]`, `[0, ..xs]`, the range `1..10` (inclusive) |
| map, ordered by key | `#{ "a": 1, "b": 2 }` |
| set, ordered | `#[1, 2, 3]` |
| record | `User { name: "A", age: 3 }`, anonymous `{ x: 1, y: 2 }` |
| record update | `user.{ age: 4 }` |
| variant | `Some(1)`, `None`, `Ok(x)`, `Err(e)`, `Circle(2)` |
| closure | `{ x -> x * 2 }`, `{ a, b -> a + b }`, `{ -> 42 }` |

Every value is immutable. There is no null, no implicit conversion
(`int.to_float(n)`), and `+` is arithmetic only: strings are built by
interpolation. Strings, lists and maps are read with module functions
(`list.get(xs, 0)`, `map.get(m, k)`); there is no `xs[i]`.

## Bindings and functions

```silt
import list

let limit = 10 -- a top-level value, bound once

fn clamp(x: Int, hi: Int) -> Int {
  match {
    x > hi -> hi
    _ -> x
  }
}

fn main() {
  let x = 42
  let x = x + 1 -- shadows: a new binding
  let (a, b) = (1, 2) -- an irrefutable pattern
  let double = { n -> n * 2 }
  let total = [1, 2, 3] |> list.fold(0) { acc, n -> acc + n }
  println("{clamp(x, limit)} {double(a + b)} {total}") -- 10 6 6
}
```

- `fn name(param: Type, ...) -> Ret where a: Trait { body }`: the last
  expression is the value; `return v` leaves early. Annotations are
  optional; types are inferred.
- Functions are top-level only. A local helper is a closure,
  `let f = { n -> ... }`, which may call itself by its name.
- A closure in the last argument may follow the call: `list.map(xs) { x ->
  x + 1 }`.
- `x |> f(y)` is `f(x, y)`.
- An expression before a block's last has to be `()`: discard any other
  value with `let _ = ...`.
- `expr as Type` states a type the checker cannot infer: `[] as List(Int)`.

## Types

```
type User { name: String, age: Int }       -- a record
type Shape { Circle(Int), Rect(Int, Int) } -- an enum
type Tree(a) { Leaf, Node(Tree(a), a, Tree(a)) }
type Names = List(String)                   -- an alias
Fn(Int, Int) -> Bool                        -- a function type
{ name: String, ...r }                      -- an open anonymous record
```

A lowercase name in an annotation is a type variable: `fn first(xs:
List(a)) -> Option(a)`. A function whose result type is chosen by its
caller takes the type as a last parameter, `fn parse(s: String, type a)`,
called as `json.parse(text, User)`.

## Branching and loops

`match` is the only branching construct, and it must be exhaustive.

```silt
fn describe(xs: List(Int)) -> String {
  match xs {
    [] -> "empty"
    [x] when x < 0 -> "one negative"
    [0, ..] | [1, ..] -> "starts small"
    [x, ..rest] -> "{x} and {rest}"
  }
}

fn grade(n: Int) -> String {
  match n {
    90..100 -> "A"
    _ -> "B or below"
  }
}

fn main() {
  println(describe([5, 6])) -- 5 and [6]
  println(grade(95)) -- A
}
```

- Patterns: literals, `_`, names, variants `Some(x)` and `m.Variant(x)`,
  tuples, records `User { name, .. }`, lists `[a, b]`, `[h, ..t]`,
  maps `#{ "k": v }`, ranges `1..9`, or-patterns `a | b`, guards
  `p when cond`, and pins `^x` (equal to the existing `x`).
- A `match` without a subject has conditions for patterns (`x > 0 ->
  ...`): it takes the first arm whose condition is true, and ends with a
  `_ -> ...` arm.
- `when let Some(x) = e else { return d }` binds `x` for the rest of the
  block; `when cond else { ... }` checks a condition. The `else` block
  must leave: `return`, `panic(...)`, a call of a function that returns
  `Never`, or `loop(...)`.
- `loop i = 0, acc = [] { ... loop(i + 1, next) ... }` binds state and
  starts again where `loop(...)` is called, in tail position; its value
  is that of a round that does not call it.

## Errors

Errors are values: `Result(a, e)` and `Option(a)`. `e?` gives the `Ok` or
`Some` value, or returns the `Err` or `None` from the function, whose
error type must be the same (`?` converts nothing). A `?` at the end of
a pipeline applies to all of it. The error enums of the standard library
have the `Error` trait (`Error.message(e)`). `panic(msg)` stops the
program. A function declared `-> Never` (the one place `Never` is
written) never returns either: its body ends in `panic`, in a call of
another such function or in an endless `loop`.

## Traits

```silt
import list

type Temp {
  degrees: Int,
}

trait Describe {
  fn describe(self) -> String
}

trait Describe for Temp {
  fn describe(self) -> String {
    "{self.degrees} degrees"
  }
}

trait Display for Temp {
  fn display(self) -> String {
    "{self.degrees}C"
  }
}

fn all(xs: List(a)) -> List(String) where a: Describe {
  list.map(xs, Describe.describe)
}

fn main() {
  let t = Temp { degrees: 21 }
  println(Describe.describe(t)) -- 21 degrees
  println(all([t])) -- [21 degrees]
  println(t) -- 21C
}
```

- A method is called through its trait, `Describe.describe(t)`; a dot
  after a value is always a field. A method that makes a value is called
  on a type: `Int.empty()`, or `a.empty()` for a type variable with the
  bound (`where a: Monoid`) or a `type a` parameter.
- Bounds: `where a: Equal + Hash, b: Display`. A supertrait:
  `trait Loud: Describe { ... }`. A trait may give default bodies,
  associated types (`type Item`, `Self::Item`) and parameters
  (`trait From(a)`, implemented as `trait From(Int) for Temp`).
- An impl is written in the module of its type or of its trait, and is
  never imported.
- Built in: `Display` (by structure, or a written impl), `Equal`, `Hash`
  and `Compare` (always by structure; an impl of them is an error),
  `Error` (written), `Number` (`Int` and `Float`). `Functor`,
  `Applicative` and `Monad` are traits of type constructors (see
  [Functors](language/functors.md)). A record compares
  field by field in declared order, an enum by variant order, then
  payload.

## Concurrency

`task.spawn { -> ... }` starts a task and gives its `Handle(a)`;
`task.join(h)` waits for its value. Tasks talk over channels:
`channel.new(n)`, `channel.send`, `channel.receive` (`None` once closed
and empty), `channel.close`, `channel.select`. I/O inside a task does not
block the others. A failed task that nobody joins ends the program with
status 1.

## Tests

A file named `*_test.silt` holds tests: each `fn test_*()` runs, and
`skip_test_*` is skipped. Assertions are in the `test` module
(`test.assert_eq(actual, expected)`). `silt test` runs them all.

## Operators, loosest first

```
||   &&   == !=   < > <= >=   |>   ..   + -   * / %   -x !x   as   f(...) ?   .
```

All infix operators are left-associative.

## Tools

`silt run`, `check`, `test`, `fmt`, `repl` (`:type <expr>`, `:doc
<name>`), `lsp`, `init`, `add`, `update`. `-w` re-runs `run`, `check`,
`test` and `disasm` when a file changes.

## Standard library

Everything but the prelude is reached through its module after `import
m`: `list.map(xs, f)`, `io.NotFound(path)`, `time.Weekday`. Each module
lists its types, traits and opaque types first, then its functions and
constants. A function of a cargo feature is in builds with that feature.

<!-- Generated from the builtin registry from here on: SILT_BLESS=1 cargo nextest run --test meta language_card -->

### Prelude

```
type Result(a, e) { Ok(a), Err(e) }
type Option(a) { Some(a), None }
type Bytes  -- opaque
trait Functor { fn map(xs: Self(a), f: Fn(a) -> b) -> Self(b) }
trait Applicative: Functor { fn pure(x: a) -> Self(a); fn map2(xs: Self(a), ys: Self(b), f: Fn(a, b) -> c) -> Self(c) }
trait Monad: Applicative { fn flat_map(xs: Self(a), f: Fn(a) -> Self(b)) -> Self(b) }
panic(message: a) -> Never where a: Display
print(value: a) -> () where a: Display
println(value: a) -> () where a: Display
```

### io

```
type IoError { NotFound(String), PermissionDenied(String), AlreadyExists(String), InvalidInput(String), Interrupted, UnexpectedEof, WriteZero, Other(String) }
io.args() -> List(String)
io.eprint(value: a) -> () where a: Display
io.eprintln(value: a) -> () where a: Display
io.inspect(x: a) -> String
io.read_file(path: String) -> Result(String, IoError)
io.read_line() -> Result(String, IoError)
io.write_file(path: String, contents: String) -> Result((), IoError)
```

### string

```
string.byte_length(s: String) -> Int
string.char_code(s: String) -> Option(Int)
string.chars(s: String) -> List(String)
string.contains(s: String, sub: String) -> Bool
string.ends_with(s: String, suffix: String) -> Bool
string.from(x: a) -> String where a: Display
string.from_char_code(code: Int) -> Option(String)
string.index_of(s: String, needle: String) -> Option(Int)
string.is_alnum(s: String) -> Bool
string.is_alpha(s: String) -> Bool
string.is_digit(s: String) -> Bool
string.is_empty(s: String) -> Bool
string.is_lower(s: String) -> Bool
string.is_upper(s: String) -> Bool
string.is_whitespace(s: String) -> Bool
string.join(xs: List(String), sep: String) -> String
string.last_index_of(s: String, needle: String) -> Option(Int)
string.length(s: String) -> Int
string.lines(s: String) -> List(String)
string.pad_left(s: String, width: Int, pad: String) -> String
string.pad_right(s: String, width: Int, pad: String) -> String
string.repeat(s: String, n: Int) -> String
string.replace(s: String, from: String, to: String) -> String
string.slice(s: String, start: Int, end: Int) -> String
string.split(s: String, separator: String) -> List(String)
string.split_at(s: String, idx: Int) -> Option((String, String))
string.starts_with(s: String, prefix: String) -> Bool
string.starts_with_at(s: String, offset: Int, prefix: String) -> Bool
string.to_lower(s: String) -> String
string.to_upper(s: String) -> String
string.trim(s: String) -> String
string.trim_end(s: String) -> String
string.trim_start(s: String) -> String
```

### int

```
type IntError { Empty, InvalidDigit(Int), Overflow, Underflow, Other(String) }
int.abs(n: Int) -> Int
int.clamp(x: Int, lo: Int, hi: Int) -> Int
int.max(a: Int, b: Int) -> Int
int.min(a: Int, b: Int) -> Int
int.parse(s: String) -> Result(Int, IntError)
int.to_float(n: Int) -> Float
```

### float

```
type FloatError { Empty, InvalidDigit(Int), Overflow, Underflow, NotFinite, Other(String) }
float.abs(f: Float) -> Float
float.ceil(f: Float) -> Float
float.clamp(x: Float, lo: Float, hi: Float) -> Float
float.epsilon: Float
float.floor(f: Float) -> Float
float.format(f: Float, decimals: Int) -> String
float.max(a: Float, b: Float) -> Float
float.max_value: Float
float.min(a: Float, b: Float) -> Float
float.min_positive: Float
float.min_value: Float
float.parse(s: String) -> Result(Float, FloatError)
float.round(f: Float) -> Float
float.to_int(f: Float) -> Int
```

### list

```
type Step(a) { Stop(a), Continue(a) }
list.all(xs: List(a), f: Fn(a) -> Bool) -> Bool
list.any(xs: List(a), f: Fn(a) -> Bool) -> Bool
list.append(xs: List(a), elem: a) -> List(a)
list.concat(xs: List(a), ys: List(a)) -> List(a)
list.contains(xs: List(a), elem: a) -> Bool where a: Equal
list.drop(xs: List(a), n: Int) -> List(a)
list.each(xs: List(a), f: Fn(a) -> ()) -> ()
list.enumerate(xs: List(a)) -> List((Int, a))
list.filter(xs: List(a), f: Fn(a) -> Bool) -> List(a)
list.filter_map(xs: List(a), f: Fn(a) -> Option(b)) -> List(b)
list.find(xs: List(a), f: Fn(a) -> Bool) -> Option(a)
list.flat_map(xs: List(a), f: Fn(a) -> List(b)) -> List(b)
list.flatten(xs: List(List(a))) -> List(a)
list.fold(xs: List(a), init: b, f: Fn(b, a) -> b) -> b
list.fold_until(xs: List(a), init: b, f: Fn(b, a) -> Step(b)) -> b
list.get(xs: List(a), i: Int) -> Option(a)
list.group_by(xs: List(a), f: Fn(a) -> b) -> Map(b, List(a)) where b: Hash
list.head(xs: List(a)) -> Option(a)
list.index_of(xs: List(a), target: a) -> Option(Int) where a: Equal
list.intersperse(xs: List(a), sep: a) -> List(a)
list.is_empty(xs: List(a)) -> Bool
list.last(xs: List(a)) -> Option(a)
list.length(xs: List(a)) -> Int
list.map(xs: List(a), f: Fn(a) -> b) -> List(b)
list.max_by(xs: List(a), key: Fn(a) -> b) -> Option(a) where b: Compare
list.min_by(xs: List(a), key: Fn(a) -> b) -> Option(a) where b: Compare
list.prepend(xs: List(a), elem: a) -> List(a)
list.product(xs: List(Int)) -> Int
list.product_float(xs: List(Float)) -> Float
list.remove_at(xs: List(a), index: Int) -> Option(List(a))
list.reverse(xs: List(a)) -> List(a)
list.scan(xs: List(a), init: b, f: Fn(b, a) -> b) -> List(b)
list.sequence(xs: List(m(a))) -> m(List(a)) where m: Applicative
list.set(xs: List(a), index: Int, value: a) -> Option(List(a))
list.sort(xs: List(a)) -> List(a) where a: Compare
list.sort_by(xs: List(a), key: Fn(a) -> b) -> List(a) where b: Compare
list.sort_with(xs: List(a), cmp: Fn(a, a) -> Int) -> List(a)
list.sum(xs: List(Int)) -> Int
list.sum_float(xs: List(Float)) -> Float
list.tail(xs: List(a)) -> List(a)
list.take(xs: List(a), n: Int) -> List(a)
list.traverse(xs: List(a), f: Fn(a) -> m(b)) -> m(List(b)) where m: Applicative
list.unfold(seed: a, f: Fn(a) -> Option((b, a))) -> List(b)
list.unique(xs: List(a)) -> List(a) where a: Equal
list.unique_by(xs: List(a), key: Fn(a) -> b) -> List(a) where b: Hash
list.zip(xs: List(a), ys: List(b)) -> List((a, b))
```

### map

```
map.contains(m: Map(a, b), key: a) -> Bool where a: Hash
map.delete(m: Map(a, b), key: a) -> Map(a, b) where a: Hash
map.each(m: Map(a, b), f: Fn(a, b) -> ()) -> ()
map.entries(m: Map(a, b)) -> List((a, b))
map.filter(m: Map(a, b), f: Fn(a, b) -> Bool) -> Map(a, b)
map.from_entries(entries: List((a, b))) -> Map(a, b) where a: Hash
map.get(m: Map(a, b), k: a) -> Option(b) where a: Hash
map.is_empty(m: Map(a, b)) -> Bool
map.keys(m: Map(a, b)) -> List(a)
map.length(m: Map(a, b)) -> Int
map.map(m: Map(a, b), f: Fn(a, b) -> (c, d)) -> Map(c, d) where c: Hash
map.merge(m1: Map(a, b), m2: Map(a, b)) -> Map(a, b) where a: Hash
map.set(m: Map(a, b), k: a, v: b) -> Map(a, b) where a: Hash
map.update(m: Map(a, b), key: a, default: b, f: Fn(b) -> b) -> Map(a, b) where a: Hash
map.values(m: Map(a, b)) -> List(b)
```

### result

```
result.flat_map(r: Result(a, b), f: Fn(a) -> Result(c, b)) -> Result(c, b)
result.flatten(r: Result(Result(a, b), b)) -> Result(a, b)
result.is_err(r: Result(a, b)) -> Bool
result.is_ok(r: Result(a, b)) -> Bool
result.map(r: Result(a, b), f: Fn(a) -> c) -> Result(c, b)
result.map_err(r: Result(a, b), f: Fn(b) -> c) -> Result(a, c)
result.unwrap_or(r: Result(a, b), default: a) -> a
```

### option

```
option.flat_map(opt: Option(a), f: Fn(a) -> Option(b)) -> Option(b)
option.is_none(opt: Option(a)) -> Bool
option.is_some(opt: Option(a)) -> Bool
option.map(opt: Option(a), f: Fn(a) -> b) -> Option(b)
option.to_result(opt: Option(a), error: b) -> Result(a, b)
option.unwrap_or(opt: Option(a), default: a) -> a
```

### test

```
test.assert(condition: Bool) -> ()
test.assert_approx(a: Float, b: Float, tolerance: Float) -> ()
test.assert_eq(left: a, right: a) -> () where a: Equal + Display
test.assert_ne(left: a, right: a) -> () where a: Equal + Display
```

### channel

```
type ChannelResult(a) { Message(a), Empty, Closed }
type ChannelOp(a) { Recv(Channel(a)), Send(Channel(a), a) }
type Selected(a) { Received(Channel(a), Option(a)), Sent(Channel(a)) }
channel.close(ch: Channel(a)) -> ()
channel.each(ch: Channel(a), f: Fn(a) -> ()) -> ()
channel.new(capacity: Int) -> Channel(a)
channel.receive(ch: Channel(a)) -> Option(a)
channel.receive_timeout(ch: Channel(a), dur: Duration) -> ChannelResult(a)
channel.select(ops: List(ChannelOp(a))) -> Selected(a)
channel.send(ch: Channel(a), value: a) -> ()
channel.timeout(dur: Duration) -> Channel(a)
channel.try_receive(ch: Channel(a)) -> ChannelResult(a)
channel.try_send(ch: Channel(a), value: a) -> Bool
```

### task

```
type TaskError { Timeout, Other(String) }
type Handle(a)  -- opaque
task.cancel(handle: Handle(a)) -> ()
task.deadline(dur: Duration, f: Fn() -> a) -> Result(a, TaskError)
task.join(handle: Handle(a)) -> a
task.spawn(f: Fn() -> a) -> Handle(a)
```

### regex

```
type RegexError { InvalidPattern(String, Int), TooBig, Other(String) }
regex.captures(text: String, pattern: String) -> Result(Option(List(String)), RegexError)
regex.captures_all(text: String, pattern: String) -> Result(List(List(String)), RegexError)
regex.captures_named(text: String, pattern: String) -> Result(Option(Map(String, String)), RegexError)
regex.find(text: String, pattern: String) -> Result(Option(String), RegexError)
regex.find_all(text: String, pattern: String) -> Result(List(String), RegexError)
regex.is_match(text: String, pattern: String) -> Result(Bool, RegexError)
regex.replace(text: String, pattern: String, replacement: String) -> Result(String, RegexError)
regex.replace_all(text: String, pattern: String, replacement: String) -> Result(String, RegexError)
regex.replace_all_with(text: String, pattern: String, f: Fn(String) -> String) -> Result(String, RegexError)
regex.split(text: String, pattern: String) -> Result(List(String), RegexError)
```

### json

```
type JsonError { Syntax(String, Int), TypeMismatch(String, String), MissingField(String), Other(String) }
type Value { Null, Bool(Bool), Int(Int), Float(Float), String(String), Array(List(Value)), Object(Map(String, Value)) }
trait Encode { fn encode(self) -> Value }
trait Decode { fn decode(value: Value) -> Result(Self, JsonError) }
json.decode(value: Value, type a) -> Result(a, JsonError) where a: Decode
json.encode(value: a) -> Value where a: Encode
json.parse(s: String, type a) -> Result(a, JsonError) where a: Decode
json.pretty(value: a) -> String where a: Encode
json.stringify(value: a) -> String where a: Encode
```

### toml

```
type TomlError { Syntax(String, Int), TypeMismatch(String, String), MissingField(String), Other(String) }
toml.parse(s: String, type a) -> Result(a, TomlError) where a: json.Decode
toml.parse_list(s: String, type a) -> Result(List(a), TomlError) where a: json.Decode
toml.parse_map(s: String, type a) -> Result(Map(String, a), TomlError) where a: json.Decode
toml.pretty(value: a) -> Result(String, TomlError) where a: json.Encode
toml.stringify(value: a) -> Result(String, TomlError) where a: json.Encode
```

### set

```
set.contains(s: Set(a), elem: a) -> Bool where a: Hash
set.difference(a: Set(a), b: Set(a)) -> Set(a)
set.each(s: Set(a), f: Fn(a) -> ()) -> ()
set.filter(s: Set(a), f: Fn(a) -> Bool) -> Set(a)
set.fold(s: Set(a), init: b, f: Fn(b, a) -> b) -> b
set.from_list(xs: List(a)) -> Set(a) where a: Hash
set.insert(s: Set(a), elem: a) -> Set(a) where a: Hash
set.intersection(a: Set(a), b: Set(a)) -> Set(a)
set.is_empty(s: Set(a)) -> Bool
set.is_subset(a: Set(a), b: Set(a)) -> Bool
set.length(s: Set(a)) -> Int
set.map(s: Set(a), f: Fn(a) -> b) -> Set(b) where b: Hash
set.new() -> Set(a)
set.remove(s: Set(a), elem: a) -> Set(a) where a: Hash
set.symmetric_difference(a: Set(a), b: Set(a)) -> Set(a)
set.to_list(s: Set(a)) -> List(a)
set.union(a: Set(a), b: Set(a)) -> Set(a)
```

### math

```
math.acos(x: Float) -> Float
math.asin(x: Float) -> Float
math.atan(x: Float) -> Float
math.atan2(y: Float, x: Float) -> Float
math.cos(x: Float) -> Float
math.e: Float
math.exp(x: Float) -> Float
math.log(x: Float) -> Float
math.log10(x: Float) -> Float
math.pi: Float
math.pow(base: Float, exponent: Float) -> Float
math.random() -> Float
math.sin(x: Float) -> Float
math.sqrt(x: Float) -> Float
math.tan(x: Float) -> Float
```

### time

```
type Instant { epoch_ns: Int }
type Date { year: Int, month: Int, day: Int }
type Time { hour: Int, minute: Int, second: Int, ns: Int }
type DateTime { date: Date, time: Time }
type Duration { ns: Int }
type Weekday { Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday }
type TimeError { ParseFormat(String), OutOfRange(String), InvalidFormat(String), Other(String) }
time.add(instant: Instant, duration: Duration) -> Instant
time.add_days(date: Date, days: Int) -> Date
time.add_months(date: Date, months: Int) -> Date
time.date(year: Int, month: Int, day: Int) -> Result(Date, TimeError)
time.datetime(date: Date, time: Time) -> DateTime
time.days_between(from: Date, to: Date) -> Int
time.days_in_month(year: Int, month: Int) -> Int
time.format(datetime: DateTime, pattern: String) -> Result(String, TimeError)
time.format_date(date: Date, pattern: String) -> Result(String, TimeError)
time.hours(n: Int) -> Duration
time.is_leap_year(year: Int) -> Bool
time.micros(n: Int) -> Duration
time.minutes(n: Int) -> Duration
time.ms(n: Int) -> Duration
time.nanos(n: Int) -> Duration
time.now() -> Instant
time.parse(s: String, pattern: String) -> Result(DateTime, TimeError)
time.parse_date(s: String, pattern: String) -> Result(Date, TimeError)
time.seconds(n: Int) -> Duration
time.since(from: Instant, to: Instant) -> Duration
time.sleep(duration: Duration) -> ()
time.time(hour: Int, min: Int, sec: Int) -> Result(Time, TimeError)
time.to_datetime(instant: Instant, offset_minutes: Int) -> DateTime
time.to_instant(datetime: DateTime, offset_minutes: Int) -> Instant
time.today() -> Date
time.weekday(date: Date) -> Weekday
```

### http

```
type Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS }
type Response { status: Int, body: Bytes, headers: Map(String, String) }
type Request { method: Method, path: String, query: String, headers: Map(String, String), body: Bytes }
type HttpError { Connect(String), Tls(String), Timeout, InvalidUrl(String), InvalidResponse(String), ClosedEarly, Other(String) }
http.get(url: String) -> Result(Response, HttpError)  -- cargo feature `http`
http.parse_query(query: String) -> Result(Map(String, List(String)), EncodingError)
http.request(method: Method, url: String, body: Bytes, headers: Map(String, String)) -> Result(Response, HttpError)  -- cargo feature `http`
http.segments(path: String) -> List(String)
http.serve(listener: TcpListener, handler: Fn(Request) -> Response) -> ()  -- cargo feature `http`
```

### fs

```
type FileStat { size: Int, is_file: Bool, is_dir: Bool, is_symlink: Bool, modified: Int, readonly: Bool, mode: Int, accessed: Option(DateTime), created: Option(DateTime) }
fs.copy(from: String, to: String) -> Result((), IoError)
fs.exists(path: String) -> Bool
fs.glob(pattern: String) -> Result(List(String), IoError)
fs.is_dir(path: String) -> Bool
fs.is_file(path: String) -> Bool
fs.is_symlink(path: String) -> Bool
fs.list_dir(path: String) -> Result(List(String), IoError)
fs.mkdir(path: String) -> Result((), IoError)
fs.read_link(path: String) -> Result(String, IoError)
fs.remove(path: String) -> Result((), IoError)
fs.rename(from: String, to: String) -> Result((), IoError)
fs.stat(path: String) -> Result(FileStat, IoError)
fs.walk(root: String) -> Result(List(String), IoError)
```

### env

```
env.get(key: String) -> Option(String)
env.remove(name: String) -> ()
env.set(key: String, value: String) -> ()
env.vars() -> List((String, String))
```

### postgres (cargo feature `postgres`)

```
type PostgresError { Connect(String), Tls(String), AuthFailed(String), Query(String, String), TypeMismatch(String, String, String), NoSuchColumn(String), Closed, Timeout, TxnAborted, Other(String) }
type Notification { channel: String, payload: String, pid: Int }
type PgPool  -- opaque
type PgTx  -- opaque
type PgCursor(a)  -- opaque
postgres.close(pool: PgPool) -> ()
postgres.connect(url: String) -> Result(PgPool, PostgresError)
postgres.connect_with(url: String, opts: Map(String, Int)) -> Result(PgPool, PostgresError)
postgres.cursor(tx: PgTx, sql: String, params: p, batch_size: Int, type r) -> Result(PgCursor(r), PostgresError)
postgres.cursor_close(cursor: PgCursor(r)) -> Result((), PostgresError)
postgres.cursor_next(cursor: PgCursor(r)) -> Result(List(r), PostgresError)
postgres.execute(conn: a, sql: String, params: p) -> Result(Int, PostgresError)
postgres.listen(pool: PgPool, channel: String) -> Result(Channel(Notification), PostgresError)
postgres.notify(conn: a, channel: String, payload: String) -> Result((), PostgresError)
postgres.query(conn: a, sql: String, params: p, type r) -> Result(List(r), PostgresError)
postgres.stream(conn: a, sql: String, params: p, type r) -> Result(Channel(Result(r, PostgresError)), PostgresError)
postgres.transact(pool: PgPool, f: Fn(PgTx) -> Result(a, PostgresError)) -> Result(a, PostgresError)
```

### bytes

```
type BytesError { InvalidUtf8(Int), InvalidHex(String), InvalidBase64(String), ByteOutOfRange(Int), Other(String) }
bytes.concat(a: Bytes, b: Bytes) -> Bytes
bytes.concat_all(parts: List(Bytes)) -> Bytes
bytes.empty() -> Bytes
bytes.ends_with(b: Bytes, suffix: Bytes) -> Bool
bytes.from_base64(s: String) -> Result(Bytes, BytesError)
bytes.from_hex(s: String) -> Result(Bytes, BytesError)
bytes.from_list(xs: List(Int)) -> Result(Bytes, BytesError)
bytes.from_string(s: String) -> Bytes
bytes.get(b: Bytes, i: Int) -> Option(Int)
bytes.index_of(b: Bytes, needle: Bytes) -> Option(Int)
bytes.length(b: Bytes) -> Int
bytes.slice(b: Bytes, start: Int, end: Int) -> Bytes
bytes.split(b: Bytes, sep: Bytes) -> List(Bytes)
bytes.starts_with(b: Bytes, prefix: Bytes) -> Bool
bytes.to_base64(b: Bytes) -> String
bytes.to_hex(b: Bytes) -> String
bytes.to_list(b: Bytes) -> List(Int)
bytes.to_string(b: Bytes) -> Result(String, BytesError)
```

### crypto

```
type CryptoError { InvalidLength(Int), Other(String) }
crypto.blake2b(data: Bytes) -> Bytes
crypto.constant_time_eq(a: Bytes, b: Bytes) -> Bool
crypto.hmac_sha256(key: Bytes, msg: Bytes) -> Bytes
crypto.hmac_sha512(key: Bytes, msg: Bytes) -> Bytes
crypto.md5(data: Bytes) -> Bytes
crypto.random_bytes(n: Int) -> Result(Bytes, CryptoError)
crypto.sha256(data: Bytes) -> Bytes
crypto.sha512(data: Bytes) -> Bytes
```

### encoding

```
type EncodingError { InvalidPercent(String), InvalidUtf8(String), Other(String) }
encoding.form_decode(body: String) -> Result(List((String, String)), EncodingError)
encoding.form_encode(pairs: List((String, String))) -> String
encoding.url_decode(s: String) -> Result(String, EncodingError)
encoding.url_encode(s: String) -> String
```

### tcp (cargo feature `tcp`)

```
type TcpError { Connect(String), Bind(String), Tls(String), Closed, Timeout, Other(String) }
type TcpListener  -- opaque
type TcpStream  -- opaque
tcp.accept(listener: TcpListener) -> Result(TcpStream, TcpError)
tcp.close(stream: TcpStream) -> ()
tcp.connect(addr: String) -> Result(TcpStream, TcpError)
tcp.connect_tls(addr: String, hostname: String) -> Result(TcpStream, TcpError)  -- cargo feature `tcp-tls`
tcp.listen(addr: String) -> Result(TcpListener, TcpError)
tcp.local_port(listener: TcpListener) -> Int
tcp.peer_addr(stream: TcpStream) -> Result(String, TcpError)
tcp.read(stream: TcpStream, max: Int) -> Result(Bytes, TcpError)
tcp.read_exact(stream: TcpStream, n: Int) -> Result(Bytes, TcpError)
tcp.set_nodelay(stream: TcpStream, on: Bool) -> Result((), TcpError)
tcp.tls_server(conn: TcpStream, cert_pem: Bytes, key_pem: Bytes) -> Result(TcpStream, TcpError)  -- cargo feature `tcp-tls`
tcp.tls_server_mtls(conn: TcpStream, cert_pem: Bytes, key_pem: Bytes, client_ca_pem: Bytes) -> Result(TcpStream, TcpError)  -- cargo feature `tcp-tls`
tcp.write(stream: TcpStream, data: Bytes) -> Result((), TcpError)
```

### stream

```
stream.buffered(ch: Channel(a), n: Int) -> Channel(a)
stream.chunks(ch: Channel(a), n: Int) -> Channel(List(a))
stream.collect(ch: Channel(a)) -> List(a)
stream.concat(chs: List(Channel(a))) -> Channel(a)
stream.count(ch: Channel(a)) -> Int
stream.dedup(ch: Channel(a)) -> Channel(a) where a: Equal
stream.drop(ch: Channel(a), n: Int) -> Channel(a)
stream.drop_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
stream.file_chunks(path: String, size: Int) -> Channel(Result(Bytes, IoError))
stream.file_lines(path: String) -> Channel(Result(String, IoError))
stream.filter(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
stream.filter_ok(ch: Channel(Result(a, b)), pred: Fn(a) -> Bool) -> Channel(Result(a, b))
stream.first(ch: Channel(a)) -> Option(a)
stream.flat_map(ch: Channel(a), f: Fn(a) -> List(b)) -> Channel(b)
stream.fold(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> b
stream.from_list(xs: List(a)) -> Channel(a)
stream.from_range(lo: Int, hi: Int) -> Channel(Int)
stream.last(ch: Channel(a)) -> Option(a)
stream.map(ch: Channel(a), f: Fn(a) -> b) -> Channel(b)
stream.map_ok(ch: Channel(Result(a, b)), f: Fn(a) -> c) -> Channel(Result(c, b))
stream.merge(chs: List(Channel(a))) -> Channel(a)
stream.repeat(x: a) -> Channel(a)
stream.scan(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> Channel(b)
stream.take(ch: Channel(a), n: Int) -> Channel(a)
stream.take_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
stream.tcp_chunks(stream: TcpStream, size: Int) -> Channel(Result(Bytes, TcpError))  -- cargo feature `tcp`
stream.tcp_lines(stream: TcpStream) -> Channel(Result(String, TcpError))  -- cargo feature `tcp`
stream.unfold(seed: a, f: Fn(a) -> Option((b, a))) -> Channel(b)
stream.write_to_file(ch: Channel(Bytes), path: String) -> Result((), IoError)
stream.write_to_tcp(ch: Channel(Bytes), stream: TcpStream) -> Result((), TcpError)  -- cargo feature `tcp`
stream.zip(a: Channel(a), b: Channel(b)) -> Channel((a, b))
```

### uuid

```
type UuidError { Invalid(String), Other(String) }
uuid.is_valid(s: String) -> Bool
uuid.nil() -> String
uuid.parse(s: String) -> Result(String, UuidError)
uuid.v4() -> String
uuid.v7() -> String
```

### process

```
process.exit(code: Int) -> Never
```
