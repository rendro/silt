# Silt Language Friction Report (Updated)

Compiled from 10 evaluation programs, 2026-03-29.
Updated to reflect all features added since the original assessment.

---

## 1. Executive Summary

**Overall Language Rating: 7.6 / 10** (up from 6.0 in the original report)

The round of improvements since the initial assessment has addressed the three most impactful pain points: the missing list/string stdlib primitives, the match-on-bool ceremony, and the match scrutinee parsing limitation. Specifically:

- **list.append / list.concat** eliminated the `flatten([acc, [item]])` workaround that appeared in 8/10 programs.
- **list.get** eliminated the 15-line recursive helper needed in 7/10 programs for indexed access.
- **string.index_of / string.slice** made text parsing viable without split-and-reassemble hacks.
- **sort_by** replaced the O(n*k) find-max-and-remove pattern with a clean O(n log n) pipeline primitive.
- **Guardless match** (`match { cond -> body, _ -> default }`) resolved the #1 language design complaint: 4 levels of nested `match bool { true -> ... false -> ... }` collapse into flat conditional blocks.
- **Match scrutinee fix** means `match x > y { true -> ... }` parses correctly now, removing the most surprising syntax trap.
- **TCO** gives stack safety guarantees for recursive loops (kvstore, todo, concurrent_processor).
- **Or-patterns, range patterns, map patterns** enrich the pattern matching system.
- **Channel module** (`channel.new`, `channel.send`, `channel.receive`, `channel.close`) provides namespaced alternatives to the bare builtins.
- **REPL** (`silt repl`) and **Formatter** (`silt fmt`) round out the tooling story.

**Remaining Top 3 Issues:**

1. **No channel iteration construct** -- `for msg in ch { ... }` is still missing; recursive drain helpers are still required for the most common concurrent pattern.
2. **Type checker still lags behind runtime** -- many builtins remain unregistered in the type checker, causing false positives that erode trust.
3. **No panic-catching / try() builtin** -- the test framework cannot express negative tests or catch expected failures.

---

## 2. Ratings by Program (Re-evaluated)

These ratings reflect what each program would score **if rewritten today** using the new features. The programs themselves have not been rewritten -- they still contain the old workarounds (flatten, recursive list_get helpers, match-on-bool). The improvement column shows what changed.

| # | Program | Old | New | Key Improvements Available |
|---|---------|:---:|:---:|---------------------------|
| 1 | link_checker.silt | 6/10 | **7.5/10** | string.index_of + string.slice eliminate the fragile split-on-"]()" parsing; guardless match replaces `match true { _ when ... }`; list.append replaces flatten workaround |
| 2 | csv_analyzer.silt | 7/10 | **8.5/10** | list.get eliminates the 15-line `list_get` recursive helper; float.min/float.max eliminate the `float_min`/`float_max` helpers; list.enumerate replaces the manual counter-via-fold; sort_by enables clean column sorting; guardless match cleans up boolean branches |
| 3 | concurrent_processor.silt | 7/10 | **7.5/10** | list.append replaces `flatten([acc, [result]])` in collect_results; TCO guarantees stack safety for worker_loop and drain recursion. Main friction (no channel iteration) remains. |
| 4 | kvstore.silt | 6/10 | **7/10** | Guardless match simplifies conditional branches; string.index_of + string.slice simplify "SET key value" parsing; TCO guarantees the recursive REPL loop is stack-safe. Accumulator threading friction remains. |
| 5 | expr_eval.silt | 7/10 | **8.5/10** | list.append/list.concat eliminate all flatten workarounds in `apply_token` and `expr_to_rpn`; or-patterns (already used) and range patterns (already used) are now properly supported. TCO helps accumulator-based evaluation. |
| 6 | todo.silt | 5/10 | **7.5/10** | list.append eliminates the pain-9/10 `flatten([todos, [todo]])` workaround; list.get provides indexed access for DONE/REMOVE by id without full-list scan; guardless match cleans up boolean formatting; TCO makes the recursive REPL safe. |
| 7 | text_stats.silt | 5/10 | **8/10** | sort_by replaces the O(n*k) find-max-and-remove `print_top_n` pattern; list.take replaces the recursive top-N helper; guardless match replaces the pervasive `let is_greater = ...; match is_greater { ... }` pattern (the comparison-as-scrutinee issue is also fixed). |
| 8 | config_parser.silt | 5/10 | **7/10** | list.append replaces `flatten([errors, [msg]])`; guardless match flattens the 4-level nested `match bool { true -> ... }` in `classify_line`; record types with generic fields now work in the type checker, so the accumulator can be a proper record instead of a 4-tuple. string.index_of simplifies bracket extraction. |
| 9 | pipeline.silt | 7/10 | **8.5/10** | list.take replaces the zip-filter-map workaround; sort_by replaces the zip-sort-extract dance in `sort_by_length`; list.append cleans up fold-based list building; list.enumerate replaces manual zip with indices. Pipe operator (already excellent) now has proper stdlib support. |
| 10 | test_suite.silt | 5/10 | **6/10** | list.append replaces `flatten([suite.results, [result]])`; record types with generic fields (List in TestSuite) now type-check. Core limitation (no panic catching) is unchanged, keeping the score from rising higher. |

**New average: 7.6 / 10** (up from 6.0)

---

## 3. Remaining Friction (What's Still Missing)

Items that were in the original report and have been **fixed** are excluded. Only genuinely missing features remain.

| Missing Feature | Programs That Would Benefit | Workaround | Impact |
|-----------------|:---------------------------:|------------|--------|
| **Channel iteration (`for msg in ch`)** | 1 (concurrent_processor) + any future concurrent code | Recursive drain helper function | **High** -- this is the #1 channel pattern (Go's `for msg := range ch`) and it still requires boilerplate |
| **`try(fn)` / panic-catching** | 1 (test_suite) + any program needing error recovery | Cannot be worked around -- failing assertions abort the program | **High** -- blocks negative tests entirely |
| **`silt test --filter <pattern>`** | 1 (test_suite) | Run all tests every time | **Medium** -- slows development iteration |
| **string.pad_left / string.pad_right** | 1 (csv_analyzer) | Manual helper with `string.repeat` | **Low** -- workaround is 5 lines |
| **list.flat_map(xs, fn)** | 1 (link_checker) | `map { ... } |> flatten` | **Low** -- two-step workaround is acceptable |
| **list.any / list.all** | 1 (csv_analyzer) | `find { ... } |> option.is_some` | **Low** -- intent is less clear than `any` but works |
| **string.strip_prefix / strip_suffix** | 1 (config_parser) | Split on bracket chars and pattern match | **Low** |
| **string.format (decimal place control)** | 1 (csv_analyzer) | Manual string manipulation of float output | **Low** |
| **Negative literal patterns (`match x { -1 -> ... }`)** | 1 (expr_eval) | Use a guard: `n when n == -1 -> ...` | **Low** |
| **Local function definitions (fn inside fn)** | 1 (expr_eval) | Define helpers at module level | **Low** |
| **`while` / `loop` construct** | 4 (kvstore, todo, concurrent_processor, text_stats) | Recursive functions with TCO | **Medium** -- TCO now guarantees stack safety, significantly reducing urgency. The ceremony of threading accumulators remains. |
| **Full type checker parity with runtime** | 4+ (text_stats, config_parser, concurrent_processor, pipeline) | Use workarounds (string.length instead of len, bind comparisons to let, use tuples instead of records) | **Medium** -- less acute now that record generics work, but many builtins still unregistered |

---

## 4. Language Design Friction (Updated)

### 4.1 Recursive Loops: How Much Does Guardless Match Help?

**Before**: Programs like config_parser.silt had 4 levels of nested `match bool { true -> ... false -> ... }` for line classification. The `match true { _ when cond -> ... }` pattern in link_checker.silt's `validate_url` was the standard workaround.

**After**: Guardless match (`match { cond -> body, _ -> default }`) collapses these into flat conditional blocks. The config_parser's `classify_line` function, which currently nests `match string.starts_with(...) { true -> match string.contains(...) { true -> ... } }`, could become:

```
match {
  trimmed == "" -> Blank
  string.starts_with(trimmed, "#") -> Comment
  string.starts_with(trimmed, "[") -> parse_section_header(trimmed, line_num)
  string.contains(trimmed, "=") -> parse_key_value(trimmed, line_num)
  _ -> ParseError("line {line_num}: unrecognized '{trimmed}'")
}
```

Similarly, link_checker.silt's `validate_url` could drop the `match true { _ when ... }` pattern entirely. This is a major readability win -- it addresses what was the #1 language design complaint in the original report.

**Impact on loop ceremony**: Guardless match does NOT help with the recursive loop pattern itself (kvstore, todo REPL loops). The accumulator-threading overhead remains. However, **TCO now guarantees stack safety**, which removes the concern about deep recursion in long-running REPL sessions. The remaining cost is purely ceremony (every branch must recurse), not correctness or safety.

### 4.2 Channel Draining: Still Recursive

The channel draining pattern is unchanged. You still write:

```
fn drain(ch) {
  match receive(ch) {
    None -> ()
    msg -> { process(msg); drain(ch) }
  }
}
```

However, three things have improved:
1. **TCO** guarantees this will not blow the stack, even for thousands of messages.
2. **`channel.close` + receive returning `None`** provides a clean termination signal (this was already present but is now also available via the `channel.close` namespaced form).
3. **`try_receive` returning `Some`/`None`** provides non-blocking polling.

The remaining gap is ergonomic, not functional: a `for msg in ch { ... }` construct would eliminate the boilerplate recursive helper.

### 4.3 Accumulator Threading: Record Generics Now Work

**Before**: config_parser.silt was forced to use a 4-tuple `(config, current_section, errors, line_num)` because record types with generic fields (`Map`, `List`) caused type checker errors.

**After**: Record types with generic fields now work. The accumulator can be:

```
type ParseState {
  config: Map,
  current_section: String,
  errors: List,
  line_num: Int,
}
```

This significantly reduces the tuple ceremony. Field access (`state.config`, `state.errors`) is clearer than tuple destructuring, and record update syntax (`state.{ line_num: state.line_num + 1 }`) is more readable than rebuilding tuples.

The remaining friction is that every match arm in a command dispatch (kvstore, todo) must still produce the updated state and pass it to the recursive call. This is inherent to the immutable model and is a deliberate design choice.

---

## 5. Type Checker Issues (Updated)

### 5.1 What Has Been Fixed

- **Record types with generic fields**: Defining a record with `Map` and `List` fields no longer fails with unresolved generics. This was the single most impactful type checker fix, as it forced config_parser.silt to abandon records for tuples.
- **Match comparison scrutinees**: `match x > y { true -> ... }` now parses correctly. The `in_match_scrutinee` flag in the parser suppresses trailing-closure interpretation while allowing comparison operators.

### 5.2 Remaining False Positives

| Issue | Programs Affected | Severity |
|-------|:-----------------:|----------|
| `len()` typed as `List(a) -> Int` only, but runtime accepts strings and maps | 2 (text_stats, pipeline) | High -- using `len(string)` still corrupts downstream inference |
| Many builtins not registered in type checker | 4+ | Medium -- runtime works, checker does not know about them |

### 5.3 Builtins Still Missing from Type Checker

The following builtins work at runtime but may not be registered in the type checker:

- **list module**: `list.append`, `list.concat`, `list.get`, `list.take`, `list.drop`, `list.enumerate`
- **string module**: `string.index_of`, `string.slice`, `string.to_lower`, `string.to_upper`, `string.chars`, `string.length`, `string.repeat`
- **float module**: `float.min`, `float.max`
- **map module**: `map.get`, `map.set`, `map.keys`, `map.values`, `map.delete`, `map.merge`
- **io module**: `io.read_file`, `io.write_file`, `io.read_line`, `io.args`
- **Higher-order builtins**: `sort_by`, `flatten`

The type checker infers their types from usage context, which sometimes leads to incorrect unifications. This remains the most confusing aspect of the language for new users.

### 5.4 Is the Type Checker Closer to Parity?

Yes, incrementally. The record generics fix and the match scrutinee fix remove two of the highest-impact type checker issues. However, the bulk of the gap (unregistered builtins) remains. A systematic audit registering all builtins would close the majority of remaining false positives.

---

## 6. What Works Well (Updated)

### Pipe Operator (|>)
Universally praised across all 10 programs. Now even better with proper stdlib support (list.take, list.drop, list.enumerate, sort_by all compose naturally in pipes).

> "Pipe chains (|> filter, |> map, |> each) are genuinely delightful for list processing." -- link_checker.silt

> "The pipe operator |> is the star of this program... reads almost exactly like a Unix pipeline." -- pipeline.silt

### Guardless Match (NEW)
Eliminates the #1 language design complaint. The `match { cond -> body, _ -> default }` syntax replaces both `match true { _ when cond -> ... }` and nested `match bool { true -> ... false -> ... }` patterns. If the 10 programs were rewritten, 8/10 would use guardless match for cleaner conditional logic.

### Or-Patterns (NEW)
Used in expr_eval.silt for operator matching: `"+" | "add" -> ...`. Clean and expressive. Reduces duplicated match arms.

### Range Patterns (NEW)
Used in expr_eval.silt for complexity classification: `2..3 -> "simple"`, `4..7 -> "moderate"`. Makes numeric range matching concise.

### List Patterns
Deep list destructuring `[head, ..tail]` is powerful and used in 8/10 programs. Map patterns (`#{ "key": value }`) are now available for map destructuring.

### Trailing Closure Syntax
Praised by 8/10 programs.

> "Trailing closure syntax { x -> body } is concise and reads well, especially in map/filter/each chains." -- csv_analyzer.silt

### String Interpolation
Mentioned positively in 6/10 programs.

> "String interpolation with {expr} is clean and effortless." -- link_checker.silt

### Record Update Syntax
Rated 9/10 delight.

> "`todo.{ done: !todo.done }` is beautiful. Clear, concise, reads like English." -- todo.silt

### Pattern Matching (Deep + ADTs)
Deep pattern matching on recursive algebraic types "just works."

> "Algebraic types with recursive nesting just work out of the box." -- expr_eval.silt

### when/else Guard
Clean early-return error handling praised by 5/10 programs.

> "The when/else guard statement is great for early-return error handling, very readable compared to nested match." -- csv_analyzer.silt

### Channel Primitives
Clean, minimal concurrency model with good type inference. Now available in both bare form (`chan`, `send`, `receive`, `close`) and namespaced form (`channel.new`, `channel.send`, `channel.receive`, `channel.close`).

> "Channel creation is minimal and clean: `chan()` and `chan(10)` -- no type annotations needed." -- concurrent_processor.silt

### TCO (NEW)
Tail-call optimization via trampolining in `call_closure`. Guarantees stack safety for recursive REPL loops (kvstore, todo) and channel drain patterns (concurrent_processor). Removes the concern about stack depth that was noted in the original expr_eval.silt report.

### REPL (NEW)
`silt repl` provides an interactive environment for exploring the language. Useful for quick experimentation and learning.

### Formatter (NEW)
`silt fmt` provides consistent code formatting. Reduces style debates and makes code review easier.

### Test Runner Basics
Zero-config test discovery remains praised.

> "`silt test` with test_ prefix convention is dead simple. Zero config, zero boilerplate." -- test_suite.silt

---

## 7. Recommended Improvements (Updated -- Remaining Items Only)

### Quick Wins (< 1 day each)

| # | Improvement | Programs Helped | Effort |
|---|-------------|:---------------:|--------|
| 1 | Register all runtime builtins in the type checker | 4+ | Tedious but straightforward -- the single most impactful remaining improvement |
| 2 | `string.pad_left` / `string.pad_right` | 1 | Add two builtins |
| 3 | `list.flat_map(xs, fn)` | 1 | Add single builtin |
| 4 | `list.any(xs, fn)` / `list.all(xs, fn)` | 1 | Add two builtins |
| 5 | `string.strip_prefix` / `string.strip_suffix` | 1 | Add two builtins |
| 6 | Fix `len()` to be typed for String and Map in type checker | 2 | Type checker registration |

### Medium Effort (1-3 days)

| # | Improvement | Programs Helped | Notes |
|---|-------------|:---------------:|-------|
| 1 | `for msg in channel { ... }` iteration | 1+ concurrent programs | New syntax for channel draining -- the #1 remaining concurrency ergonomic gap |
| 2 | `try(fn)` builtin returning `Result` (catch panics) | 1 (test_suite) | Enables negative tests and error recovery |
| 3 | `silt test --filter <pattern>` | 1 (test_suite) | CLI flag for selective test running |
| 4 | Test setup/teardown hooks | 1 (test_suite) | Convention-based `test_setup()` function |

### Large Effort (1+ weeks)

| # | Improvement | Programs Helped | Notes |
|---|-------------|:---------------:|-------|
| 1 | Full type checker parity with runtime | all | Systematic audit and registration of all builtins with correct type signatures |
| 2 | `while` / `loop` construct with break | 4 | Lower urgency now that TCO is implemented -- the remaining cost is ceremony, not correctness |
| 3 | Regex or pattern matching on strings | 2 | Significant stdlib addition; would transform link_checker and config_parser |
| 4 | List indexing syntax `xs[n]` | 7 | Parser + runtime change; `list.get` covers the functional need but `xs[n]` is more ergonomic |
| 5 | `if/else` expression | 8 | Lower urgency now that guardless match exists -- guardless match covers 90% of the use cases |

**Note**: Several items from the original "Quick Wins" and "Medium Effort" lists have been completed and are no longer listed: list.append, list.concat, list.get, string.index_of, string.slice, float.min/max, list.take/drop, list.enumerate, sort_by, match scrutinee fix, record generic fields fix.

---

## 8. Feature Coverage Matrix (Updated)

New columns are marked with **(NEW)** in the header. An "x" means the feature is used in the program. A "w" means the program uses an old workaround that the new feature would replace.

| Feature | link_checker | csv_analyzer | concurrent_proc | kvstore | expr_eval | todo | text_stats | config_parser | pipeline | test_suite |
|---------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Pipe operator `\|>` | x | x | x | - | x | x | x | - | x | x |
| Trailing closures | x | x | x | - | x | x | x | x | x | x |
| String interpolation | x | x | x | x | x | x | x | x | x | x |
| Pattern matching (basic) | x | x | x | x | x | x | x | x | x | x |
| Pattern matching (nested) | - | - | - | - | x | - | - | - | - | x |
| Or-patterns `\|` **(NEW)** | - | - | - | - | x | - | - | - | - | - |
| Range patterns `n..m` **(NEW)** | - | - | - | - | x | - | - | - | - | - |
| Map patterns `#{}` **(NEW)** | - | - | - | - | - | - | - | - | - | - |
| Pattern matching (guards) | x | - | - | - | - | - | - | - | - | x |
| Guardless match **(NEW)** | w | w | - | w | - | w | w | w | w | - |
| List destructuring `[h, ..t]` | x | x | x | x | - | x | - | x | - | x |
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
| `sort_by` **(NEW)** | - | w | - | - | - | - | w | - | w | - |
| `list.append` **(NEW)** | - | - | w | - | w | w | - | w | w | w |
| `list.concat` **(NEW)** | - | - | - | - | w | - | - | - | - | - |
| `list.get` **(NEW)** | - | w | - | - | - | w | w | w | w | - |
| `list.take` / `list.drop` **(NEW)** | - | - | - | - | - | - | w | - | w | - |
| `list.enumerate` **(NEW)** | - | w | - | - | - | - | - | - | - | - |
| `string.index_of` **(NEW)** | w | - | - | w | - | - | - | w | - | - |
| `string.slice` **(NEW)** | w | - | - | w | w | w | - | - | - | - |
| `float.min` / `float.max` **(NEW)** | - | w | - | - | - | - | - | - | - | - |
| Map data structure `#{}` | - | x | - | x | - | - | x | x | - | x |
| `io.read_file` / `io.write_file` | x | x | x | x | - | x | x | x | x | - |
| `io.read_line` (interactive) | - | - | - | x | - | x | - | - | - | - |
| `io.args` (CLI args) | x | x | - | - | - | - | x | - | - | - |
| Channels (`chan`, `send`, `receive`) | - | - | x | - | - | - | - | - | - | - |
| `channel.*` namespaced **(NEW)** | - | - | x | - | - | - | - | - | - | - |
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
| TCO **(NEW)** | - | - | x | x | - | x | - | - | - | - |
| REPL (`silt repl`) **(NEW)** | - | - | - | - | - | - | - | - | - | - |
| Formatter (`silt fmt`) **(NEW)** | - | - | - | - | - | - | - | - | - | - |
| `assert` / `assert_eq` | - | - | - | - | - | - | - | - | - | x |
| `panic` | - | - | - | - | x | x | - | - | - | x |
| Block comments `{- -}` | x | x | x | x | x | x | x | x | x | x |
| Expression-body fn `fn f(x) = expr` | - | - | - | - | x | - | - | - | - | - |

**Legend**: **x** = feature is used; **w** = program uses an old workaround that this new feature would replace; **-** = not used/not applicable.

---

## 9. Changelog: What Was Fixed Since the Original Report

For reference, here is the complete list of improvements made since the original assessment:

| Feature | Impact | Programs Affected |
|---------|--------|:-----------------:|
| `list.append(xs, x)` | Eliminates `flatten([acc, [item]])` | 8 |
| `list.concat(a, b)` | Proper list concatenation | 8 |
| `list.get(xs, n)` | Eliminates recursive index helpers | 7 |
| `list.take(xs, n)` | Eliminates zip-filter-map workaround | 3 |
| `list.drop(xs, n)` | Complement to list.take | 3 |
| `list.enumerate(xs)` | Eliminates manual `zip(0..(len(xs)), xs)` | 2 |
| `string.index_of(s, needle)` | Enables proper text parsing | 4 |
| `string.slice(s, start, end)` | Enables substring extraction | 4 |
| `float.min` / `float.max` | Eliminates manual comparison helpers | 2 |
| `sort_by(list, key_fn)` | Replaces O(n*k) find-max-and-remove | 3 |
| Guardless match | Replaces nested match-on-bool | 8 |
| Match scrutinee comparison fix | `match x > y { ... }` now parses | 3 |
| Record types with generic fields | Records can contain Map/List fields | 1 |
| Or-patterns (`a \| b -> ...`) | Reduces duplicated match arms | 1+ |
| Range patterns (`1..10 -> ...`) | Concise numeric range matching | 1+ |
| Map patterns (`#{ "k": v }`) | Map destructuring in match | 0 (new capability) |
| TCO (tail-call optimization) | Stack safety for recursive loops | 4 |
| REPL (`silt repl`) | Interactive exploration | all |
| Formatter (`silt fmt`) | Consistent code formatting | all |
| Channel module (`channel.*`) | Namespaced channel operations | 1+ |

---

*The language has made significant progress. The original top-3 issues (list.append, list.get, type checker parity) are 2/3 resolved. The remaining priorities are: (1) type checker parity with runtime builtins, (2) channel iteration construct, (3) try/catch for the test framework.*
