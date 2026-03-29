# Silt Language Friction Report

Compiled from 10 evaluation programs, 2026-03-28.

---

## 1. Executive Summary

**Overall Language Rating: 6.0 / 10** (average of all 10 program ratings)

Silt's core design is coherent and pleasant for pure-functional data transformation: pipe chains, trailing closures, pattern matching, string interpolation, and immutable-by-default semantics compose well and feel modern. The language punches above its weight for a 17-keyword design. However, significant friction emerges in three areas: (1) the standard library is missing critical list and string primitives (list.append, list.get, string.index_of, string.slice, sort_by) that force multi-line workarounds for operations that should be one-liners; (2) the type checker lags far behind the runtime, with many builtins untyped or incorrectly typed, causing false-positive errors that block valid programs; (3) the absence of if/else and while/for loops creates ceremony in stateful programs where match-on-bool and recursive accumulator-threading are the only options. The runtime itself is solid -- nearly all friction is in the ergonomics layer above it.

**Top 3 most impactful improvements:**

1. **Add list.append / list.concat / `++` operator** -- needed by 8/10 programs, currently requires `flatten([acc, [item]])` everywhere
2. **Add list.get(xs, n) or indexing syntax xs[n]** -- needed by 7/10 programs, currently requires recursive helpers or pattern matching chains
3. **Bring type checker to parity with runtime builtins** -- hit by 4/10 programs, causes false errors that erode trust in the compiler

---

## 2. Ratings by Program

| # | Program | Rating | Primary Friction |
|---|---------|--------|------------------|
| 1 | link_checker.silt | 6/10 | No string.index_of / string.slice makes text parsing fragile and verbose |
| 2 | csv_analyzer.silt | 7/10 | No list indexing (list.get / xs[n]) forces recursive helper for column access |
| 3 | concurrent_processor.silt | 7/10 | No channel iteration construct; must write recursive drain helpers |
| 4 | kvstore.silt | 6/10 | Accumulator-threading through recursive REPL loop is error-prone |
| 5 | expr_eval.silt | 7/10 | No list concat operator; flatten workaround for building lists |
| 6 | todo.silt | 5/10 | No list.append (pain 9/10) and no list.get (pain 8/10) combined |
| 7 | text_stats.silt | 5/10 | No sort_by, no list.take; type checker blocks valid programs |
| 8 | config_parser.silt | 5/10 | Record types with generic fields fail type checker; must use tuples |
| 9 | pipeline.silt | 7/10 | No list concat, take, or sort_by; pipe operator itself is excellent |
| 10 | test_suite.silt | 5/10 | No panic-catching; no test filtering; test runner is too minimal |

---

## 3. Missing Stdlib Functions (ranked by frequency)

| Missing Function | Programs Needing It | Workaround Used | Impact |
|-----------------|--------------------:|-----------------|--------|
| **list.append / list.concat / `++`** | 8 | `flatten([acc, [item]])` | **High** -- appears in nearly every program; the flatten workaround is ugly and non-obvious |
| **list.get(xs, n) / xs[n]** | 7 | Recursive helper function (15+ lines) or chained head/tail | **High** -- critical for CSV columns, split results, any indexed access |
| **string.index_of(s, needle)** | 4 | Split on delimiter, pattern match on parts | **High** -- makes any text parsing beyond simple delimiters extremely painful |
| **string.slice(s, start, end)** | 4 | Split + rejoin or chars + manual extraction | **High** -- paired with index_of, would enable proper substring operations |
| **sort_by(list, key_fn)** | 3 | Zip with sort key, sort tuples, map to extract originals | **Medium** -- O(n*k) find-max-and-remove workaround vs O(n log n) |
| **list.take(xs, n) / list.drop(xs, n)** | 3 | Zip with indices, filter i < n, map to extract | **Medium** -- common pipeline operation |
| **float.min / float.max** | 2 | Manual `match a < b { true -> a, false -> b }` helpers | **Medium** -- surprising gap given int.min/int.max exist |
| **list.enumerate(xs)** | 2 | `zip(0..(len(xs)), xs)` | **Low** -- workaround is reasonable but verbose |
| **string.pad_left / string.pad_right** | 1 | Manual helper with string.repeat | **Low** -- only needed for table formatting |
| **list.flat_map(xs, fn)** | 1 | `map |> flatten` | **Low** -- two-step workaround is acceptable |
| **list.any / list.all** | 1 | `find { ... } |> option.is_some` | **Low** -- workaround exists but intent is unclear |
| **string.strip_prefix / strip_suffix** | 1 | Split on bracket chars and pattern match | **Low** |
| **string.format (decimal places)** | 1 | Manual string manipulation of float-to-string output | **Low** |

---

## 4. Language Design Friction

### 4.1 Match-Only Branching (no if/else)

**Hit by: 8/10 programs** -- every program with conditional logic beyond pattern matching.

The `match true { _ when cond -> ... }` or `match cond { true -> ... false -> ... }` pattern is the universal workaround. It works but has two distinct problems:

1. **Readability**: Matching on `true` with wildcard patterns and guards reads oddly. You are pattern matching against a value you do not care about just to access the guard syntax. Programs like config_parser.silt have 4 levels of nested `match bool { true -> ... false -> ... }` for what would be a flat if/else-if/else chain.

2. **Syntax trap**: The text_stats.silt report discovered that **comparison expressions cannot be match scrutinees** because the match parser uses binding power 116 while comparisons have bp=60. Writing `match x > y { true -> ... }` is a parse error. You must bind to a let first: `let gt = x > y; match gt { ... }`. This is the single most surprising syntax issue reported.

**Verdict**: Match-as-branching works for 2-3 cases but scales poorly. A `cond` expression or guardless match (no scrutinee) would significantly reduce nesting.

### 4.2 Recursive Loops (no while/for)

**Hit by: 4/10 programs** (kvstore, todo, concurrent_processor, text_stats).

The recursive loop pattern has three specific pain points:

1. **Every branch must recurse**: In kvstore.silt and todo.silt, each of 8+ match arms must remember to call the recursive loop function with the updated state. Forgetting means silent termination -- there is no compiler warning.

2. **Accumulator threading**: The immutable model forces passing state as arguments: `repl(todos, next_id)` in todo.silt threads two values. config_parser.silt threads a 4-tuple `(config, current_section, errors, line_num)`. This is manageable for small state but scales poorly.

3. **No guaranteed TCO**: The expr_eval.silt report notes that recursive calls not in tail position (computing sub-expressions then combining) do not benefit from TCO. For large workloads, stack depth is a concern.

**Verdict**: Functional programs (map/filter/fold chains) are unaffected. Stateful REPL-style programs (kvstore, todo) bear the highest cost. A `loop` or `while` construct would halve the ceremony for these cases.

### 4.3 Immutable Accumulator Threading

**Hit by: 6/10 programs** (kvstore, todo, config_parser, text_stats, csv_analyzer, concurrent_processor).

The pattern of threading state through every code path manifests in several ways:

- **Fold with wide tuples**: config_parser.silt folds with `(config, current_section, errors, line_num)` -- destructuring this on every iteration is ceremonial.
- **Record update saves the day**: When state is a record, `record.{ field: new_value }` is elegant (todo.silt rates this 9/10 delight). But record types with generic fields (Map, List) fail the type checker (config_parser.silt), forcing tuples instead.
- **Every match arm must produce state**: In command-dispatch patterns (kvstore, todo), every arm that modifies state must produce the new state and pass it to the recursive call. Miss one arm and the program silently terminates.

### 4.4 Channel Draining (recursive receive loop)

**Hit by: 1/10 programs** (concurrent_processor) but would affect any concurrent program.

The concurrent_processor.silt report identifies this as the **single biggest ergonomic gap in the concurrency model**. Go's `for msg := range ch { ... }` is the #1 channel pattern, and Silt has no equivalent. Instead you must write:

```
fn drain(ch) {
  match receive(ch) {
    None -> ()
    msg -> { process(msg); drain(ch) }
  }
}
```

This recursive helper must be written for every "consume until closed" pattern. Additionally, `receive()` returns `None` on close while `try_receive()` returns `None` for both "empty" and "closed", making it impossible to distinguish the two without blocking.

**Verdict**: Adding `for msg in ch { ... }` (loop until close) would push concurrency ergonomics from 7/10 to 9/10.

---

## 5. Type Checker Issues

### 5.1 False Positives

| Issue | Programs Affected | Severity |
|-------|:-----------------:|----------|
| `len()` typed as `List(a) -> Int` only, but runtime accepts strings and maps | 2 (text_stats, pipeline) | High -- using `len(string)` corrupts downstream type inference |
| `string.replace` typed as `(String, String) -> String` but runtime takes 3 args | 1 (concurrent_processor) | Medium |
| `flatten`, `string.length`, `string.repeat` not in type checker at all | 2 (concurrent_processor, text_stats) | Medium -- works at runtime, unknown to checker |
| Record types with generic fields (e.g., `Map`, `List` in record field types) fail with unresolved generics | 1 (config_parser) | High -- forces abandoning records for tuples in accumulators |

### 5.2 Missing Runtime Features from Type Checker

The following builtins work at runtime but are not registered in the type checker (reported by text_stats.silt and concurrent_processor.silt):

`io.read_file`, `io.args`, `map.get`, `map.set`, `map.keys`, `map.delete`, `string.to_lower`, `string.chars`, `int.to_float`, `float.round`, `flatten`, `string.length`, `string.repeat`

The type checker infers their types from usage context, which sometimes leads to incorrect unifications.

### 5.3 How Often Did Type Errors Block Valid Programs?

- **text_stats.silt**: Had to use `string.length(s)` instead of `len(s)` for strings, and bind comparisons to `let` before matching. Two workarounds in a single program.
- **config_parser.silt**: Abandoned record type for accumulator state because `Map` and `List` without generic parameters are not valid type expressions in record field annotations. Fell back to tuples.
- **concurrent_processor.silt**: Could not use `string.replace` with 3 args; had to restructure string processing logic.

**Verdict**: The gap between what the interpreter supports and what the type checker knows is "the single most confusing aspect of the language" (text_stats.silt). Programs that avoid the gaps work fine; programs that hit them face confusing errors with no clear path forward.

---

## 6. What Works Well

### Pipe Operator (|>)
Universally praised across all 10 programs. Every program that processes lists uses pipe chains, and they consistently read naturally.

> "Pipe chains (|> filter, |> map, |> each) are genuinely delightful for list processing." -- link_checker.silt

> "The pipe operator |> is the star of this program... reads almost exactly like a Unix pipeline." -- pipeline.silt

> "Building `lines |> grep("ERROR") |> uppercase() |> numbered()` reads almost exactly like a Unix pipeline." -- pipeline.silt

### Trailing Closure Syntax
Praised by 8/10 programs. The `{ x -> body }` syntax makes map/filter/fold calls feel lightweight.

> "Trailing closure syntax { x -> body } is concise and reads well, especially in map/filter/each chains." -- csv_analyzer.silt

### String Interpolation
Mentioned positively in 6/10 programs. `{expr}` inside strings avoids concatenation noise.

> "String interpolation with {expr} is clean and effortless." -- link_checker.silt

### Record Update Syntax
Rated 9/10 delight by todo.silt.

> "`todo.{ done: !todo.done }` is beautiful. Clear, concise, reads like English." -- todo.silt

### Pattern Matching
Deep pattern matching on nested constructors "just works" (expr_eval.silt). Or-patterns for operator matching (`"+" | "add" -> ...`) are elegant. List destructuring `[head, ..tail]` is powerful.

> "Algebraic types with recursive nesting just work out of the box." -- expr_eval.silt

> "Pattern matching on nested constructors is natural and readable." -- expr_eval.silt

### when/else Guard
Clean early-return error handling praised by 5/10 programs.

> "The when/else guard statement is great for early-return error handling, very readable compared to nested match." -- csv_analyzer.silt

### Channel Primitives (Concurrency)
Clean, minimal design with good type inference.

> "Channel creation is minimal and clean: `chan()` and `chan(10)` -- no type annotations needed." -- concurrent_processor.silt

> "`receive()` returning None on a closed channel is elegant -- it gives you a clean termination signal." -- concurrent_processor.silt

### Test Runner Basics
Zero-config test discovery is praised.

> "`silt test` with test_ prefix convention is dead simple. Zero config, zero boilerplate." -- test_suite.silt

---

## 7. Recommended Improvements (Prioritized)

### Quick Wins (< 1 day each)

| # | Improvement | Programs Helped | Effort |
|---|-------------|:---------------:|--------|
| 1 | `list.append(xs, x)` / `list.push(xs, x)` | 8 | Add single builtin |
| 2 | `list.get(xs, n)` returning `Option` | 7 | Add single builtin |
| 3 | `string.index_of(s, needle)` returning `Option(Int)` | 4 | Add single builtin |
| 4 | `string.slice(s, start, end)` | 4 | Add single builtin |
| 5 | `float.min` / `float.max` | 2 | Add two builtins (mirrors int.min/max) |
| 6 | `list.take(xs, n)` / `list.drop(xs, n)` | 3 | Add two builtins |
| 7 | `list.enumerate(xs)` | 2 | Add single builtin |
| 8 | `string.pad_left` / `string.pad_right` | 1 | Add two builtins |
| 9 | `list.flat_map(xs, fn)` | 1 | Add single builtin |
| 10 | Register all runtime builtins in the type checker | 4 | Tedious but straightforward |

### Medium Effort (1-3 days)

| # | Improvement | Programs Helped | Notes |
|---|-------------|:---------------:|-------|
| 1 | `sort_by(list, key_fn)` or `sort_with(list, cmp_fn)` | 3 | Requires extending sort implementation |
| 2 | List concat operator `++` or `list.concat(a, b)` | 8 | Language-level operator or builtin |
| 3 | Allow comparison expressions as match scrutinees (lower bp threshold) | 3 | Parser change to bp handling |
| 4 | `for msg in channel { ... }` iteration | 1+ | New syntax for channel draining |
| 5 | `try(fn)` builtin returning `Result` (catch panics) | 1 | Enables negative tests and error recovery |
| 6 | `silt test --filter <pattern>` | 1 | CLI flag for test runner |
| 7 | Fix record types with generic fields in type checker | 1 | Allow `Map(K, V)` / `List(T)` in field annotations |

### Large Effort (1+ weeks)

| # | Improvement | Programs Helped | Notes |
|---|-------------|:---------------:|-------|
| 1 | `if/else` or `cond` expression | 8 | Fundamental syntax addition; most impactful quality-of-life change |
| 2 | `while` / `loop` construct with break | 4 | Eliminates recursive REPL boilerplate |
| 3 | Full type checker parity with runtime | all | Systematic audit and registration of all builtins |
| 4 | Regex or pattern matching on strings | 2 | Significant stdlib addition |
| 5 | List indexing syntax `xs[n]` | 7 | Parser + runtime change; overlaps with list.get builtin |

---

## 8. Feature Coverage Matrix

| Feature | link_checker | csv_analyzer | concurrent_proc | kvstore | expr_eval | todo | text_stats | config_parser | pipeline | test_suite |
|---------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Pipe operator `\|>` | x | x | x | - | x | x | x | - | x | x |
| Trailing closures | x | x | x | - | x | x | x | x | x | x |
| String interpolation | x | x | x | x | x | x | x | x | x | x |
| Pattern matching (basic) | x | x | x | x | x | x | x | x | x | x |
| Pattern matching (nested) | - | - | - | - | x | - | - | - | - | x |
| Pattern matching (or `\|`) | - | - | - | - | x | - | - | - | - | - |
| Pattern matching (guards) | x | - | - | - | - | - | - | - | - | x |
| List destructuring `[h, ..t]` | x | x | x | x | - | x | - | x | - | x |
| Range patterns `n..m` | - | - | - | - | x | - | - | - | - | - |
| Record types | - | x | x | - | - | x | - | - | - | x |
| Record update `.{ }` | - | x | - | - | - | x | - | - | - | x |
| Algebraic data types (ADT) | - | - | - | - | x | - | - | x | - | x |
| Recursive ADTs | - | - | - | - | x | - | - | - | - | - |
| Traits (Display) | - | - | - | - | x | - | - | - | - | x |
| `when/else` early return | x | x | - | - | x | - | x | x | - | - |
| `?` operator (Result prop.) | - | - | - | - | - | - | - | x | - | - |
| Closures / HOFs | x | x | x | - | - | x | x | x | x | x |
| `map` / `filter` / `fold` | x | x | x | x | x | x | x | x | x | x |
| `find` | - | x | - | - | - | x | - | - | x | x |
| `flatten` | x | - | x | - | x | x | - | x | x | x |
| `zip` | x | - | - | - | - | - | - | - | x | x |
| `each` | x | x | x | x | x | x | - | x | x | - |
| Map data structure `#{}` | - | x | - | x | - | - | x | x | - | x |
| `io.read_file` / `io.write_file` | x | x | x | x | - | x | x | x | x | - |
| `io.read_line` (interactive) | - | - | - | x | - | x | - | - | - | - |
| `io.args` (CLI args) | x | x | - | - | - | - | x | - | - | - |
| Channels (`chan`, `send`, `receive`) | - | - | x | - | - | - | - | - | - | - |
| `spawn` / `join` | - | - | x | - | - | - | - | - | - | - |
| `select` | - | - | x | - | - | - | - | - | - | - |
| `try_receive` | - | - | x | - | - | - | - | - | - | - |
| `string.split` / `string.join` | x | x | x | x | x | x | x | x | x | x |
| `string.contains` / `starts_with` | x | x | - | x | - | - | - | x | x | x |
| `string.to_upper` / `to_lower` | - | - | - | - | - | - | x | - | x | x |
| `string.replace` | - | - | - | - | - | - | - | - | - | x |
| `string.chars` | - | x | - | - | - | - | x | - | - | x |
| `int.parse` / `float.parse` | - | x | - | - | x | x | - | x | - | x |
| `int.to_float` | - | x | - | - | - | - | x | - | - | - |
| `list.sort` | - | - | - | - | - | - | - | - | x | x |
| `list.reverse` | - | - | - | - | - | - | - | - | x | x |
| `list.head` / `list.tail` / `list.last` | x | x | - | - | - | - | - | - | - | x |
| Recursive loop (REPL) | - | - | x | x | - | x | - | - | - | - |
| `assert` / `assert_eq` | - | - | - | - | - | - | - | - | - | x |
| `panic` | - | - | - | - | x | x | - | - | - | x |
| Block comments `{- -}` | x | x | x | x | x | x | x | x | x | x |
| Expression-body fn `fn f(x) = expr` | - | - | - | - | x | - | - | - | - | - |

---

*This report will drive the next round of improvements. Priorities should be: (1) stdlib quick wins that reduce flatten/recursive-helper boilerplate, (2) type checker parity, (3) conditional/loop syntax additions.*
