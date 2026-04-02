# Silt Language Friction Report

Generated: 2026-04-01
Method: 10 programs implemented from scratch by agents with no prior silt experience.

## Executive Summary

**Average rating: 8.1 / 10.** Silt is a highly learnable language. All 10 agents successfully wrote working programs (50-277 lines each, 2,069 lines total) with an average of just 2.1 edit-run cycles. One program worked on the first attempt. The pipe operator, trailing closures, pattern matching, and `loop`-with-state are consistently praised as natural and expressive. The top friction sources are: parser limitations around inline match arms, undocumented `io.args()` behavior, homogeneous map typing, and missing convenience stdlib functions (`list.sum`, `list.min_by`, `string.strip_chars`).

## Per-Program Results

| # | Program | Rating | Attempts | Highlight | Primary Friction |
|---|---------|:------:|:--------:|-----------|-----------------|
| 1 | todo.silt | 8/10 | 2 | Loop-with-state REPL pattern; record update syntax | Guardless match arms can't be comma-separated on one line |
| 2 | pipeline.silt | 8/10 | 2 | Pipe chains; higher-order function factories | Inline match inside trailing closures causes parse errors |
| 3 | expr_eval.silt | 9/10 | 1 | Or-patterns; recursive ADT; Display trait | Minor: no list spread operator |
| 4 | config_parser.silt | 7/10 | 3 | ADT line classification; list.enumerate + fold | Homogeneous maps can't hold mixed-type values; `#{}` inside match arms confuses parser |
| 5 | csv_analyzer.silt | 8/10 | 2 | list.group_by; sort_by; string.pad_right alignment | No string concatenation operator; no list.sum/min_by/max_by |
| 6 | kvstore.silt | 9/10 | 1 | Loop-with-state for REPL; json.stringify/parse roundtrip | No case-insensitive match; no string.split_whitespace |
| 7 | concurrent_processor.silt | 8/10 | 2 | channel.select with ^pin; deterministic scheduling | Typechecker false-positive on non-exhaustive match for Closed/Message |
| 8 | text_stats.silt | 8/10 | 2 | list.fold + map for word frequencies; string.pad_right | io.args() includes binary/subcommand/script, not just user args |
| 9 | test_suite.silt | 8/10 | 4 | try() + test.assert_eq for custom test runner | Semicolons don't work (newline-only); list.tail returns List not Option |
| 10 | link_checker.silt | 8/10 | 2 | regex.captures + list patterns; guardless match | Type inference issue with inline option.unwrap_or in comparisons |

**Average: 8.1 / 10, 2.1 attempts**

## Recurring Friction Patterns

Ranked by frequency (number of programs affected):

1. **Match arm parsing issues** (6/10) — Inline match expressions with comma-separated arms don't parse. Match inside trailing closures fails. `#{}` literal inside match arms confuses the parser. Every agent eventually learned to use newline-separated arms or hoist match into `let` bindings.

2. **Missing convenience stdlib functions** (4/10) — `list.sum`, `list.min_by`, `list.max_by`, `string.strip_chars`, `string.split_whitespace`, `map.contains_key`, `regex.captures_all`. Agents worked around these with `list.fold`, but the boilerplate was noted.

3. **No string concatenation operator** (3/10) — No `++` or `+` for strings. Agents used interpolation (`"{a}{b}"`) or `string.join` as workarounds, which works but feels indirect for simple concatenation.

4. **Homogeneous map typing** (2/10) — Maps can't hold mixed-type values. Agents building complex accumulators had to use tuples instead of maps, which is less readable.

5. **`io.args()` includes interpreter argv** (2/10) — The docs say "command-line arguments" but the list includes the binary name, subcommand, and script path. User args start at index 3, not 0.

6. **Typechecker exhaustiveness false positives** (2/10) — `Closed` + `Message(x)` should be exhaustive for `channel.receive`, and `Ok(x)` + `Err(e)` for Result, but the typechecker sometimes requires a `_` wildcard arm.

## What Felt Natural

These features were consistently praised across 5+ programs:

- **Pipe operator `|>` with trailing closures** — Universally loved. Every agent used it extensively and found it readable and composable. The `{ x -> expr }` syntax for closures outside parens was called "elegant" multiple times.

- **Pattern matching as sole branching** — Once agents internalized that there's no if/else, guardless `match { cond -> ... }` became a natural replacement. Deep matching on nested constructors (ADTs, tuples, lists) was consistently praised.

- **`loop` expression with state** — The `loop store = #{} { ... loop(new_store) }` pattern for REPLs was discovered independently by multiple agents and described as "a natural functional REPL idiom."

- **Record types and update syntax** — `u.{ age: 31 }` was called "clean" and "ergonomic" by every agent that used records.

- **String interpolation** — `"hello {name}"` with embedded expressions was universally praised as convenient and natural.

- **Algebraic data types** — Clean definition syntax, exhaustive pattern matching, or-patterns (`Add(l,r) | Mul(l,r)`) — all worked smoothly.

- **Error handling via Result/Option** — The `when`/`else` guard, `?` operator, and `try()` builtin gave agents multiple ergonomic options for error handling.

## What Felt Forced

Patterns that consistently required workarounds:

- **Boolean negation in filter contexts** — Without `if/else`, rejecting items requires `match { true -> false, _ -> true }` or extracting into a helper function. A `not()` builtin or clearer `!` operator would help. (Some agents didn't discover that `!` exists.)

- **Accumulating heterogeneous state** — Fold accumulators that track multiple types (a map + a string + a list) must use tuples, e.g., `(config_map, current_section, errors)`. This works but is less readable than a record accumulator.

- **No string concatenation** — Building strings piecemeal requires interpolation (`"{a}{b}"`) which is fine for simple cases but awkward in loops or when concatenating from a variable.

- **Match verbosity for simple conditions** — `match { x > 5 -> "big", _ -> "small" }` is more verbose than a ternary or if/else for trivial branches. This was noted but not considered a major issue.

## Missing Standard Library Functions

Consolidated from all agents:

| Function | Requested by | Use case |
|----------|-------------|----------|
| `list.sum` | csv_analyzer, text_stats | Sum a list of numbers without manual fold |
| `list.min_by` / `list.max_by` | csv_analyzer | Find min/max element by key function |
| `list.count` | link_checker | Count elements matching a predicate |
| `string.split_whitespace` / `string.words` | kvstore, text_stats | Split on any whitespace, not just a single delimiter |
| `string.strip_chars` | text_stats | Strip specific characters (like Python's `str.strip(chars)`) |
| `map.contains_key` | kvstore | Check key existence without Option dance |
| `regex.captures_all` | link_checker | Get all capture groups, not just first match |
| `option.unwrap` | various | Unwrap without a default (panic on None) |
| `channel.each` / `channel.drain` | concurrent_processor | Iterate channel values until closed |
| Tuple index access (`t.0`) | test_suite | Access tuple elements without destructuring |

## Bugs Encountered

1. **Parser: inline match arms with commas don't parse** — `match expr { arm1, arm2 }` on a single line fails. Must use newline-separated arms. (6 agents hit this)

2. **Parser: match inside trailing closures** — `list.filter { x -> match x { ... } }` can cause parse errors. Workaround: extract match into a `let` binding. (2 agents)

3. **Parser: `#{}` inside match arms** — Empty map literal `#{}` inside a match arm body confuses the parser because `}` is ambiguous (match arm end vs map literal end). (1 agent)

4. **Typechecker: false non-exhaustive match** — Matching on `channel.receive` with `Closed` + `Message(x)` arms is flagged as non-exhaustive, requiring a `_ ->` wildcard. Same for some Result matches. (2 agents)

5. **Type inference: inline `option.unwrap_or` in comparisons** — `option.unwrap_or(x, "") == "valid"` causes the typechecker to infer `Option(?N)` instead of `String`. Binding to a `let` first works around it. (1 agent)

6. **`io.args()` documentation gap** — Returns full argv including binary name, subcommand, and script path. First user argument is at index 3. The docs don't mention this. (2 agents)

## Language Snapshot

### Keywords (14)
```
let  fn  type  trait  match  when  return
pub  mod  import  as  else  where  loop
```

### Globals (13)
```
print  println  panic  try
Ok  Err  Some  None
Stop  Continue  Message  Closed  Empty
```

### Module builtins (110+ across 14 modules)

| Module | Functions | Notable |
|--------|:---------:|---------|
| `list` | 28 | map, filter, fold, fold_until, unfold, sort_by, group_by, enumerate |
| `string` | 16 | split, join, trim, contains, replace, pad_left, pad_right, slice |
| `map` | 11 | get, set, delete, keys, values, merge, entries, from_entries |
| `int` | 6 | parse, abs, min, max, to_float, to_string |
| `float` | 10 | parse, round, ceil, floor, abs, to_string (1-2 args), to_int |
| `io` | 5 | read_file, write_file, read_line, args, inspect |
| `result` | 6 | map_ok, map_err, unwrap_or, flatten, is_ok, is_err |
| `option` | 5 | map, unwrap_or, to_result, is_some, is_none |
| `channel` | 7 | new, send, receive, close, select, try_send, try_receive |
| `task` | 3 | spawn, join, cancel |
| `regex` | 7 | is_match, find, find_all, captures, split, replace, replace_all |
| `json` | 3 | parse, stringify, pretty |
| `math` | 13 | sqrt, pow, log, trig functions, pi, e |
| `test` | 3 | assert, assert_eq, assert_ne |

### Codebase metrics

| Metric | Value |
|--------|-------|
| Rust source (src/*.rs) | 15,193 lines |
| Rust tests | 157 |
| Silt programs written | 10 files, 2,069 lines |
| Largest source file | typechecker.rs (5,138 lines) |
| Interpreter | interpreter.rs (3,822 lines) |

## Code Showcases

### 1. Expression evaluator — recursive ADT with or-patterns and Display trait

```silt
type Expr {
  Num(Int)
  Add(Expr, Expr)
  Mul(Expr, Expr)
  Neg(Expr)
}

trait Display for Expr {
  fn display(self) -> String {
    match self {
      Num(n) -> "{n}"
      Add(l, r) -> "({l.display()} + {r.display()})"
      Mul(l, r) -> "({l.display()} * {r.display()})"
      Neg(e) -> "(-{e.display()})"
    }
  }
}

fn simplify(expr) {
  match expr {
    Num(n) -> Num(n)
    Add(l, r) -> {
      let sl = simplify(l)
      let sr = simplify(r)
      match (sl, sr) {
        (e, Num(0)) -> e
        (Num(0), e) -> e
        (Num(a), Num(b)) -> Num(a + b)
        (a, b) -> Add(a, b)
      }
    }
    Mul(l, r) -> {
      let sl = simplify(l)
      let sr = simplify(r)
      match (sl, sr) {
        (_, Num(0)) | (Num(0), _) -> Num(0)
        (e, Num(1)) | (Num(1), e) -> e
        (Num(a), Num(b)) -> Num(a * b)
        (a, b) -> Mul(a, b)
      }
    }
    Neg(e) -> match simplify(e) {
      Neg(inner) -> inner
      s -> Neg(s)
    }
  }
}
```

### 2. Pipeline — higher-order function factories with pipe chains

```silt
fn make_grep(pattern) = fn(lines) { grep(lines, pattern) }
fn make_reject(pattern) = fn(lines) { reject(lines, pattern) }

fn main() {
  let lines = read_lines("programs/log.txt")

  -- Chain 10 operations: grep, uppercase, numbered
  lines
  |> make_grep("ERROR")
  |> uppercase
  |> numbered
  |> list.each { line -> println(line) }

  -- Unique log levels, sorted
  lines
  |> list.map { line ->
    let parts = line |> string.split(" ")
    list.get(parts, 3) |> option.unwrap_or("UNKNOWN")
  }
  |> list.unique
  |> list.sort
  |> list.each { level -> println(level) }
}
```

### 3. Concurrent processor — channel.select with ^pin, worker loop

```silt
fn worker(id, jobs, results) {
  loop {
    match channel.receive(jobs) {
      Closed -> channel.send(results, ("done", id, 0, 0))
      Message(path) -> {
        match io.read_file(path) {
          Ok(content) -> {
            let (file_path, lines, words) = analyze(path, content)
            channel.send(results, ("ok", id, lines, words))
          }
          Err(e) -> channel.send(results, ("err", id, 0, 0))
          _ -> ()
        }
        loop()
      }
      _ -> ()
    }
  }
}

-- channel.select with ^pin to identify which channel fired
match channel.select([urgent_ch, status_ch]) {
  (^urgent_ch, msg) -> println("Urgent: {msg}")
  (^status_ch, msg) -> println("Status: {msg}")
  _ -> panic("unexpected")
}
```

### 4. Test runner — try() wrapping test.assert_eq for pass/fail tracking

```silt
fn run_test(name, test_fn) {
  match try(test_fn) {
    Ok(_) -> {
      println("[PASS] {name}")
      (1, 0)
    }
    Err(e) -> {
      println("[FAIL] {name}: {e}")
      (0, 1)
    }
  }
}

fn main() {
  let results = [
    run_test("arithmetic", fn() {
      test.assert_eq(1 + 1, 2)
      test.assert_eq(10 % 3, 1)
    }),
    run_test("lists", fn() {
      test.assert_eq([1, 2, 3] |> list.map { x -> x * 2 }, [2, 4, 6])
      test.assert_eq(list.length([]), 0)
    }),
    ...
  ]
  let passed = results |> list.fold(0) { acc, r -> let (p, _) = r; acc + p }
  let failed = results |> list.fold(0) { acc, r -> let (_, f) = r; acc + f }
  println("{passed}/{passed + failed} passed, {failed} failed")
}
```

## Verdict

Silt delivers on its promise of being a language you can learn in an afternoon. Ten agents — none of which had seen silt before — wrote 2,069 lines of working code across programs spanning REPL apps, data processing pipelines, recursive ADTs, concurrent task systems, and self-testing frameworks. The average program worked in 2.1 attempts, and two programs ran correctly on the first try. That's a strong learnability signal.

The language's core design decisions pay off. The pipe operator with trailing closures is the standout feature — every agent gravitated toward it naturally, and it makes data transformation code read like a specification of intent rather than a sequence of operations. Pattern matching as the sole branching construct is initially surprising but quickly becomes intuitive, especially with guardless match as an if/else replacement. The `loop` expression with state threading is an elegant functional alternative to mutable iteration, and multiple agents independently discovered the `loop state = init { ... loop(new_state) }` REPL pattern without being told about it.

The friction is real but manageable. The parser has rough edges around inline match expressions (comma-separated arms, match inside closures, `#{}` inside match arms) that tripped up 6 of 10 agents. The stdlib is solid but missing convenience functions that would eliminate common fold boilerplate (`list.sum`, `list.min_by`, `string.split_whitespace`). The `io.args()` documentation gap caused two agents to fail their first run. The typechecker's false non-exhaustive warnings on `channel.receive` patterns are a papercut. None of these are showstoppers — agents worked around every issue within 1-3 additional cycles — but fixing the parser issues and adding the top-requested stdlib functions would meaningfully reduce first-contact friction.
