# Silt Language -- Final Friction Report

Compiled from 10 evaluation programs, 7 examples, and full source audit. 2026-03-29.

---

## 1. Executive Summary

**Final Rating: 8.5 / 10**

Silt is a small, expression-oriented functional language with 14 keywords, 8 globals, and 87 module-qualified builtins across 11 modules. It compiles to a tree-walking interpreter written in ~12,000 lines of Rust, backed by 242 test functions.

### What is complete

- **Module system.** Clean namespace: 8 globals, everything else module-qualified. `list.map`, `channel.send`, `task.spawn` -- no bare `map` or `spawn` polluting scope.
- **Pattern matching.** Wildcard, literal (including negative), identifier, tuple, constructor, record, list (with rest), or-pattern, range, and map patterns. Guards on every arm. Guardless match for conditional blocks.
- **Pipe operator.** First-argument insertion with trailing closure syntax. The defining feature of Silt's ergonomics.
- **Concurrency primitives.** Buffered channels, cooperative tasks, select, try_send/try_receive. Full fan-out/fan-in support.
- **Error handling.** Result/Option as first-class types, `?` operator for propagation, `when`/`else` for early return, `try()` for panic recovery.
- **Records.** Named fields, immutable update syntax (`record.{ field: value }`), pattern matching on record shape.
- **Traits.** Method dispatch on enum and record types. Display trait for custom formatting.
- **Algebraic data types.** Recursive ADTs, deep pattern matching on nested constructors, variant constructors as first-class functions.
- **Tooling.** REPL (`silt repl`), formatter (`silt fmt`), test runner (`silt test` with `test_` prefix discovery).
- **Type checker.** Hindley-Milner inference with all builtins registered. Option/Result generics work correctly.
- **TCO.** Tail-call optimization via trampolining. Safe recursion for REPL loops and channel drains.

### What is deliberately omitted

- No `while`/`loop`/`for` -- recursion + TCO is the model.
- No `if`/`else` -- guardless match covers conditional blocks.
- No mutation -- shadowing, record update, and map operations handle state.
- No channel iteration -- recursive drain is the pattern.

These are design choices, not gaps.

---

## 2. Per-Program Ratings

Ratings reflect the **actual current code** in each program. Many programs still carry legacy workarounds (manual `list_get`, `pad_right`, `float_min` helpers) that are now redundant -- the stdlib has `list.get`, `string.pad_left`, `float.min`. The ratings account for this: the programs work well, but could be cleaner if rewritten with the full current stdlib.

| # | Program | Rating | Highlight | Remaining friction |
|---|---------|:------:|-----------|-------------------|
| 1 | `link_checker.silt` | 7.5 | Pipe chains for link extraction and validation; `when`/`else` for CLI args and file reading | Still uses `match true { _ when ... }` instead of guardless match in `validate_url`; split-on-`](` parsing hack could use `string.index_of` + `string.slice` |
| 2 | `csv_analyzer.silt` | 8.0 | Record types for column stats; pipe + fold for aggregation; map literal for grade counting | Carries manual `list_get`, `pad_right`, `float_min`/`float_max` helpers that are now redundant; `list.get`, `string.pad_left`, `float.min` all exist |
| 3 | `concurrent_processor.silt` | 8.0 | Clean channel/task usage; fan-out worker pool; select with priority channels; try_receive polling | Recursive `worker_loop` and `collect_results` -- the channel drain ceremony is the main cost |
| 4 | `kvstore.silt` | 7.0 | Map operations for key-value store; pattern matching on command strings; serialize/deserialize | Recursive REPL with accumulator threading -- every command branch must explicitly recurse |
| 5 | `expr_eval.silt` | 9.0 | Recursive ADTs; deep pattern matching on nested constructors; or-patterns for RPN operators; range patterns for complexity classification | Uses `list.flatten([[a + b], rest])` instead of `list.append`; minor |
| 6 | `todo.silt` | 7.0 | Record update syntax (`t.{ done: !t.done }`); pipe chains for filtering; serialize/deserialize | Same REPL ceremony as kvstore; uses `list.flatten` instead of `list.append` |
| 7 | `text_stats.silt` | 7.5 | Word frequency via fold + map; pipe chains for text processing | `print_top_n` uses recursive find-max-delete when `list.sort_by` + `list.take` would work; legacy comments about type checker gaps are now outdated |
| 8 | `config_parser.silt` | 7.5 | Algebraic types for line classification; 4-tuple fold accumulator for stateful parsing; nested map construction | Nested `match ... { true -> match ... }` could be guardless match; `list.flatten([errors, [msg]])` instead of `list.append` |
| 9 | `pipeline.silt` | 9.0 | Pipe operator showcase; custom pipeline functions compose naturally with `|>`; higher-order function factories; 12 distinct pipeline demos | `sort_by_length` and `take` are manual when `list.sort_by` and `list.take` now exist |
| 10 | `test_suite.silt` | 7.5 | Self-testing mini framework; traits for result display; comprehensive stdlib coverage tests | Cannot catch assertion failures within the framework (would need `try()` integration); no test filtering or setup/teardown |

**Average: 7.8 / 10**

If all programs were rewritten to use the full current stdlib (replacing legacy helpers with `list.get`, `list.append`, `list.sort_by`, `list.take`, `string.pad_left`, `float.min`/`float.max`, and guardless match), the average would be closer to 8.5.

---

## 3. Remaining Friction -- Deliberate Design Only

Everything below is a conscious design choice, not a bug or missing feature.

### No `while`/`loop`/`for`

Silt uses recursive functions for all iteration. TCO makes this stack-safe. The cost is ceremony: every recursive loop must explicitly pass updated state as arguments, and every branch must remember to recurse. Forgetting to recurse means silent termination.

This affects: `kvstore.silt` (REPL loop), `todo.silt` (REPL loop), `concurrent_processor.silt` (worker loop, result collector), `text_stats.silt` (top-N extraction).

The upside: all state is explicit, all data flows are visible, no hidden mutation.

### No `if`/`else`

Guardless match covers this completely:

```silt
match {
  string.length(url) == 0 -> Err("empty URL")
  string.starts_with(url, "https://") -> Ok("https")
  string.starts_with(url, "http://") -> Ok("http")
  _ -> Err("invalid scheme")
}
```

Several programs still use the older `match true { _ when cond -> ... }` or `match cond { true -> ... false -> ... }` patterns because they were written before guardless match existed. These are style debt, not language friction.

### No mutation

Shadowing (`let db = map.set(db, key, val)`) and record update syntax (`todo.{ done: true }`) handle state transformation. The accumulator-threading pattern in fold and recursive loops is the main cost.

### Channel drain is recursive

No `for msg in ch { ... }` construct. The idiomatic pattern is:

```silt
fn drain(ch) {
  match channel.receive(ch) {
    None -> ()
    msg -> {
      println("Received: {msg}")
      drain(ch)
    }
  }
}
```

This is 8 lines for what Go expresses in 1 (`for msg := range ch`). TCO makes it safe. It is the single largest per-pattern ceremony cost in the language.

---

## 4. What Works Well

### Module system

Clean namespace with 8 globals and everything else module-qualified. No import required for builtins -- `list.map`, `channel.send`, `task.spawn` are always available. The `import` keyword exists for user-defined modules. The qualified names are self-documenting.

```silt
-- No imports needed. Builtins are always available.
let words = text
  |> string.split(" ")
  |> list.filter { w -> string.length(w) > 0 }
  |> list.map { w -> string.to_lower(w) }
```

### Pipes + trailing closures

The defining feature. First-argument insertion means custom functions compose naturally with the pipe operator. Trailing closure syntax (`{ x -> body }`) keeps inline transforms lightweight.

```silt
lines
  |> grep("ERROR")
  |> uppercase()
  |> numbered()
  |> list.each { line -> println(line) }
```

From `pipeline.silt` -- reads like a Unix pipeline.

### Guardless match

Flat conditional blocks without a scrutinee. Each arm is a boolean expression:

```silt
fn classify_line(line, line_num) {
  let trimmed = string.trim(line)
  match {
    trimmed == "" -> Blank
    string.starts_with(trimmed, "#") -> Comment
    string.starts_with(trimmed, "[") -> parse_section_header(trimmed, line_num)
    string.contains(trimmed, "=") -> parse_key_value(trimmed, line_num)
    _ -> ParseError("line {line_num}: unrecognized line '{trimmed}'")
  }
}
```

### Pattern matching

All pattern types work and compose:

**Or-patterns** -- match multiple alternatives in one arm:
```silt
match token {
  "+" | "add" -> match stack {
    [b, a, ..rest] -> list.flatten([[a + b], rest])
    _ -> panic("stack underflow")
  }
}
```

**Range patterns** -- match integer ranges (inclusive):
```silt
fn classify_complexity(expr) {
  let n = count_nodes(expr)
  match n {
    1 -> "trivial"
    2..3 -> "simple"
    4..7 -> "moderate"
    8..15 -> "complex"
    _ -> "very complex ({n} nodes)"
  }
}
```

**List patterns** with rest:
```silt
match segments {
  [_single] -> []
  [head, ..tail] -> process(head, tail)
  [] -> default_value
}
```

**Record patterns** with field binding and rest:
```silt
match alice {
  Person { name, age, .. } -> println("{name} is {age}")
}
```

**Constructor patterns** at arbitrary depth:
```silt
fn simplify(expr) {
  match expr {
    Add(e, Num(0)) -> simplify(e)
    Mul(Num(0), _) -> Num(0)
    Neg(Neg(inner)) -> simplify(inner)
    Add(left, right) -> Add(simplify(left), simplify(right))
    other -> other
  }
}
```

**Map patterns**:
```silt
match config {
  #{ "host": host, "port": port } -> connect(host, port)
}
```

**Negative literal patterns**:
```silt
match n {
  -1 -> "negative one"
  0 -> "zero"
  1 -> "one"
  _ -> "other"
}
```

### `when`/`else` + `?` operator

Clean error unwrapping without nesting:

```silt
when Ok(content) = io.read_file(filepath) else {
  println("Error: could not read file")
  return Err("file read failed")
}

when [_, _, filepath, .._] = args else {
  println("Usage: silt link_checker.silt <file>")
  return Err("missing argument")
}
```

The `?` operator propagates errors in expressions:

```silt
let port_result = port_line |> string.replace("port=", "") |> int.parse()
when Ok(port) = port_result else {
  return Err("invalid port number")
}
```

### Record update syntax

Immutable updates with clear syntax:

```silt
let older = alice.{ age: alice.age + 1 }
let toggled = todo.{ done: !todo.done }
let moved = point.{ x: 100, y: 200 }
```

### String interpolation

Expressions inside strings, no format specifiers:

```silt
println("  {result.path}: {result.lines} lines, {result.words} words")
println("Results: {suite.passed}/{suite.total} passed, {suite.failed} failed")
```

### `try()` for error recovery

Catches panics and returns Result:

```silt
match try(fn() { panic("boom") }) {
  Ok(_) -> println("no panic")
  Err(msg) -> println("caught: {msg}")
}
```

### TCO for safe recursion

Tail-call optimization via trampolining. Recursive REPL loops, channel drains, and recursive data processing are all stack-safe:

```silt
fn repl(db) {
  print("kvstore> ")
  match io.read_line() {
    Ok(line) -> match execute(line, db) {
      Some(new_db) -> repl(new_db)  -- TCO: safe for infinite loops
      None -> db
    }
    Err(e) -> db
  }
}
```

### Channel primitives

Type-inferred, buffered, cooperative:

```silt
let work_ch = channel.new(10)
let results_ch = channel.new(10)

let w1 = task.spawn(fn() { worker_loop(work_ch, results_ch) })

files |> list.each { path -> channel.send(work_ch, path) }
channel.close(work_ch)

select {
  receive(high_ch) as msg -> println("[high] {msg}")
  receive(low_ch) as msg -> println("[low] {msg}")
}
```

### REPL + formatter

`silt repl` for interactive exploration. `silt fmt` for consistent formatting. `silt test` for zero-config test discovery with `test_` prefix convention.

---

## 5. Language Snapshot

### Keywords (14)

`as`, `else`, `fn`, `import`, `let`, `match`, `mod`, `pub`, `return`, `select`, `trait`, `type`, `when`, `where`

(`true` and `false` are literal tokens, not keywords.)

### Globals (8)

`print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`

### Module builtins (87 across 11 modules)

| Module | Count | Functions |
|--------|:-----:|-----------|
| **list** | 24 | map, filter, each, fold, find, zip, flatten, head, tail, last, reverse, sort, sort_by, flat_map, any, all, contains, length, append, concat, get, take, drop, enumerate |
| **string** | 16 | split, join, trim, contains, replace, length, to_upper, to_lower, starts_with, ends_with, chars, repeat, index_of, slice, pad_left, pad_right |
| **map** | 7 | get, set, delete, keys, values, length, merge |
| **float** | 7 | parse, round, ceil, floor, abs, min, max |
| **result** | 6 | unwrap_or, map_ok, map_err, flatten, is_ok, is_err |
| **io** | 5 | inspect, read_file, write_file, read_line, args |
| **int** | 5 | parse, abs, min, max, to_float |
| **option** | 5 | map, unwrap_or, to_result, is_some, is_none |
| **channel** | 6 | new, send, receive, close, try_send, try_receive |
| **task** | 3 | spawn, join, cancel |
| **test** | 3 | assert, assert_eq, assert_ne |

### Pattern types (12)

Wildcard, identifier, integer (including negative), float, boolean, string, tuple, constructor, record (with rest), list (with rest), or-pattern, range, map.

### Codebase

| Metric | Count |
|--------|------:|
| Rust source files | 14 |
| Rust test functions | 242 |
| Example programs | 7 |
| Evaluation programs | 10 |

### Tooling

| Command | Purpose |
|---------|---------|
| `silt run <file>` | Run a program |
| `silt repl` | Interactive REPL |
| `silt fmt <file>` | Format source code |
| `silt test` | Run all `test_*` functions in `*_test.silt` files |
| `silt check <file>` | Type-check without running |

---

## 6. Code Showcase

Four short programs that demonstrate Silt at its best.

### FizzBuzz -- pipes + range + pattern matching

```silt
fn fizzbuzz(n) {
  match (n % 3, n % 5) {
    (0, 0) -> "FizzBuzz"
    (0, _) -> "Fizz"
    (_, 0) -> "Buzz"
    _      -> "{n}"
  }
}

fn main() {
  1..101
  |> list.map { n -> fizzbuzz(n) }
  |> list.each { s -> println(s) }
}
```

### Records + pipes -- filtering and transforming data

```silt
type User {
  name: String,
  age: Int,
  active: Bool,
}

fn birthday(user: User) -> User {
  user.{ age: user.age + 1 }
}

fn main() {
  let users = [
    User { name: "Alice", age: 30, active: true },
    User { name: "Bob", age: 25, active: false },
  ]

  users
  |> list.filter { u -> u.active }
  |> list.map { u -> birthday(u) }
  |> list.each { u ->
    println("{u.name} is now {u.age}")
  }
}
```

### Expression simplifier -- recursive ADTs + deep pattern matching

```silt
type Expr {
  Num(Int)
  Add(Expr, Expr)
  Mul(Expr, Expr)
  Neg(Expr)
}

fn simplify(expr) {
  match expr {
    Add(e, Num(0)) -> simplify(e)
    Add(Num(0), e) -> simplify(e)
    Mul(e, Num(1)) -> simplify(e)
    Mul(Num(1), e) -> simplify(e)
    Mul(_, Num(0)) -> Num(0)
    Neg(Neg(inner)) -> simplify(inner)
    Neg(Num(0)) -> Num(0)
    Add(left, right) -> Add(simplify(left), simplify(right))
    Mul(left, right) -> Mul(simplify(left), simplify(right))
    Neg(inner) -> Neg(simplify(inner))
    other -> other
  }
}
```

### Concurrent file processor -- channels + tasks + pipes

```silt
fn worker_loop(work_ch, results_ch) {
  match channel.receive(work_ch) {
    None -> ()
    path -> {
      let result = process_file(path)
      channel.send(results_ch, result)
      worker_loop(work_ch, results_ch)
    }
  }
}

fn main() {
  let files = ["readme.txt", "notes.txt", "todo.txt", "poem.txt"]
  let work_ch = channel.new(10)
  let results_ch = channel.new(10)

  let w1 = task.spawn(fn() { worker_loop(work_ch, results_ch) })
  let w2 = task.spawn(fn() { worker_loop(work_ch, results_ch) })

  files |> list.each { path -> channel.send(work_ch, path) }
  channel.close(work_ch)

  let results = collect_results(results_ch, list.length(files), [])
  task.join(w1)
  task.join(w2)

  results |> list.each { r -> println("  {r.path}: {r.lines} lines") }
}
```

---

*Silt has reached a coherent final state. Fourteen keywords, eight globals, eighty-seven module builtins. The pipe operator, pattern matching, and module system form a consistent trio. The remaining friction -- recursive loop ceremony and channel drain boilerplate -- is the cost of the language's core design decisions: no mutation, no looping constructs, expression-oriented everything. These are tradeoffs, not deficiencies.*
