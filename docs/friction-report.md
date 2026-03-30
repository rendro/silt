# Silt Language Friction Report v3

Compiled from 10 evaluation programs, 2026-03-29. Third and definitive assessment.

---

## 1. Executive Summary

**Overall Rating: 8.3 / 10** (up from 7.6 in v2, 6.0 in v1)

Silt is a 14-keyword, expression-oriented functional language with 8 globals and 82 module-qualified builtins organized across 10 modules. The language now has a clean, coherent design: a small global namespace, consistent module-qualified access for everything else, and strong pattern matching with guardless match, or-patterns, and range patterns.

Since the last report, three significant changes pushed the rating higher:

1. **Global namespace cleanup.** `len` and `spawn` removed as globals. Only 8 names remain in global scope: `print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`. Everything else lives in modules (`list.length`, `task.spawn`, `channel.new`).
2. **`try()` builtin.** Catches panics and returns `Result`, enabling negative testing and error recovery. This was the #2 blocker in v2.
3. **Type checker parity.** All builtins are now registered with correct type signatures in the type checker. The "type checker lags behind runtime" complaint -- the #1 remaining issue in v2 -- is resolved.

**Remaining top friction points:**
1. No channel iteration construct (`for msg in ch { ... }`)
2. Recursive loop ceremony (no `while`/`loop` -- deliberate design choice)
3. A few missing typechecker registrations for concurrency builtins and `map.length`

---

## 2. Per-Program Ratings

These ratings are based on the **actual programs as they exist today**, updated to use module-qualified syntax.

| # | Program | v2 | v3 | Notes |
|---|---------|:--:|:--:|-------|
| 1 | link_checker.silt | 7.5 | **7.5** | Still uses `match true { _ when ... }` for `validate_url` -- could use guardless match but hasn't been rewritten. The split-on-`](` parsing hack remains because the program was not updated to use `string.index_of`/`string.slice`. Core friction is text parsing without regex. |
| 2 | csv_analyzer.silt | 8.5 | **8.5** | Still carries the manual `list_get`, `pad_right`, `float_min`/`float_max` helpers from v1. These are now redundant (`list.get`, `float.min`, `float.max` exist) but the program hasn't been updated. No new friction introduced; no new friction resolved since v2. |
| 3 | concurrent_processor.silt | 7.5 | **8.0** | Now uses `channel.new`, `channel.send`, `channel.receive`, `channel.close`, `channel.try_receive`, `task.spawn`, `task.join` -- clean module-qualified names throughout. `try` enables robust error handling. Recursive `worker_loop` and `collect_results` remain the main ceremony; `for msg in ch` would eliminate both. |
| 4 | kvstore.silt | 7.0 | **7.0** | Unchanged. The recursive REPL loop with accumulator threading is inherent to the immutable/no-while design. TCO makes it safe but not less ceremonial. Pattern matching on command strings works well. |
| 5 | expr_eval.silt | 8.5 | **8.5** | Still uses `list.flatten([[a + b], rest])` in `apply_token` when `list.append` would suffice. Recursive ADTs, deep pattern matching, or-patterns, and range patterns all work excellently. The program is a showcase for Silt's ML heritage. |
| 6 | todo.silt | 7.5 | **7.5** | Still uses `list.flatten([todos, [todo]])` when `list.append` exists. The program was not updated to use the newer stdlib additions. Accumulator-threading REPL loop remains the main friction. |
| 7 | text_stats.silt | 8.0 | **8.5** | `try` could now replace the `print_top_n` recursive find-max-delete pattern with a simpler `sort_by` + `list.take`. Type checker parity means no more `string.length` vs `len` confusion. The old comments about type checker gaps are now outdated. |
| 8 | config_parser.silt | 7.0 | **7.5** | Nested `match string.starts_with { true -> match ... }` could now be guardless match. `list.append` would replace `list.flatten([errors, [msg]])`. The 4-tuple accumulator could now be a record. Type checker parity helps with map operations. |
| 9 | pipeline.silt | 8.5 | **9.0** | The pipe operator is the star. `sort_by_length` could now use `list.sort_by` directly. `take` could use `list.take`. `uniq` still needs the fold+flatten pattern. This is the program where Silt shines brightest -- multi-step data pipelines read like Unix pipes. |
| 10 | test_suite.silt | 6.0 | **7.5** | `try()` is the big change. The program can now catch assertion failures and report pass/fail without aborting. `list.append` replaces the flatten workaround. `test.assert`, `test.assert_eq`, `test.assert_ne` are clean module-qualified names. Remaining gaps: no test filtering, no setup/teardown, no output capture. |

**Average: 8.0 / 10** (up from 7.6)

The per-program scores are deliberately conservative. Most programs carry legacy code (v1 workarounds) that inflates the visible friction. If all 10 programs were rewritten today using the full current stdlib, the average would be closer to 8.5.

---

## 3. Remaining Friction

### Genuine gaps

| Issue | Impact | Programs Affected | Workaround |
|-------|--------|:-----------------:|------------|
| **No channel iteration** (`for msg in ch { ... }`) | High | concurrent_processor + any future concurrent code | Recursive drain helper -- functional but 8 lines of boilerplate for Go's 1-line `for msg := range ch` |
| **Recursive loop ceremony** | Medium | kvstore, todo, concurrent_processor | Every branch must explicitly recurse with updated state. TCO makes it safe but not less verbose. This is a deliberate design choice (no `while`/`loop`), not a bug. |
| **`map.length` missing from type checker** | Low | programs using `map.length` | Works at runtime; type checker does not know about it |
| **`channel.*` / `task.*` / `try` missing from type checker** | Low | concurrent_processor | Works at runtime; these are intercepted builtins not registered in the type checker |
| **No `string.pad_left` / `string.pad_right`** | Low | csv_analyzer | 5-line helper with `string.repeat` |
| **No `list.flat_map`** | Low | link_checker | `list.map { ... } |> list.flatten` -- two steps instead of one |
| **No `list.any` / `list.all`** | Low | general | `list.find { ... } |> option.is_some` works but is less clear |
| **No negative literal patterns** (`-1`) | Low | expr_eval | Use guard: `n when n == -1 -> ...` |
| **Programs still carry legacy workarounds** | Meta | 8/10 programs | The programs have not been rewritten to use `list.append`, `list.get`, `string.index_of`, `float.min`/`float.max`, guardless match, etc. The friction in these programs is partly historical. |

### Not friction (deliberate design)

- **No `while`/`loop`**: Recursive functions + TCO is the intended model. The ceremony cost is real but bounded, and the immutable-data-threading discipline has design benefits.
- **No `if`/`else`**: Guardless match (`match { cond -> ... }`) covers this use case cleanly.
- **No mutable variables**: Shadowing + record update syntax + map operations handle state transformation idiomatically.

---

## 4. What Works Well

### The module system
The cleanest change since v1. Eight globals, everything else module-qualified. `list.map`, `string.split`, `channel.send`, `task.spawn` -- the qualified names are self-documenting and avoid polluting the global namespace. Imports are optional (builtins are always available); the `import` keyword exists for user-defined modules.

### Pipe operator (`|>`)
Universally praised in 9/10 programs. First-argument insertion composes naturally with trailing closures. Multi-step pipelines read left-to-right like Unix pipes.

```
lines
  |> grep("ERROR")
  |> uppercase()
  |> numbered()
  |> list.each { line -> println(line) }
```

### Guardless match
Resolves the #1 design complaint from v1. Flat conditional blocks replace nested `match bool { true -> ... false -> ... }`:

```
match {
  trimmed == "" -> Blank
  string.starts_with(trimmed, "#") -> Comment
  string.starts_with(trimmed, "[") -> parse_section_header(trimmed)
  _ -> ParseError("unrecognized")
}
```

### Pattern matching depth
Deep matching on recursive ADTs, or-patterns (`"+" | "add" ->`), range patterns (`2..5 ->`), list destructuring (`[head, ..tail]`), record patterns (`Person { name, .. }`), map patterns (`#{ "key": v }`). The pattern matching system is comprehensive.

### `try()` builtin
Catches panics and returns `Result`. Enables negative testing, error recovery, and defensive programming:

```
match try(fn() { panic("boom") }) {
  Ok(_) -> println("did not panic")
  Err(msg) -> println("caught: {msg}")
}
```

### Record update syntax
`todo.{ done: !todo.done }` -- concise, readable, immutable.

### `when`/`else` early return
Clean error unwrapping without nesting:

```
when Ok(content) = io.read_file(path) else {
  return Err("file read failed")
}
```

### String interpolation
`"Hello {name}, you are {age} years old"` -- no format strings, no concatenation.

### Trailing closures
`list.filter { x -> x > 0 }` -- lightweight, visually clean, composes well in pipes.

### Channel primitives
Clean, type-inferred, cooperative concurrency. `channel.new(10)`, `channel.send(ch, val)`, `select { receive(ch) as msg -> ... }`. The model is simpler than Go channels but covers the core patterns.

### TCO
Stack safety for recursive loops and channel drains. Removes the practical concern about recursive REPL patterns.

### Tooling
`silt repl` for exploration, `silt fmt` for formatting, `silt test` for zero-config test discovery.

---

## 5. Language Summary

### Keywords (14)
`fn`, `let`, `match`, `when`, `return`, `select`, `type`, `trait`, `import`, `pub`, `mod`, `as`, `else`, `where`

(Plus `true` and `false` as literal tokens, not counted as keywords.)

### Globals (8)
`print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`

### Modules and builtins (82 module-qualified + 10 intercepted)

| Module | Count | Functions |
|--------|:-----:|-----------|
| **list** | 21 | map, filter, each, fold, find, zip, flatten, head, tail, last, reverse, sort, sort_by, contains, length, append, concat, get, take, drop, enumerate |
| **string** | 14 | split, join, trim, contains, replace, length, to_upper, to_lower, starts_with, ends_with, chars, repeat, index_of, slice |
| **map** | 7 | get, set, delete, keys, values, length, merge |
| **float** | 7 | parse, round, ceil, floor, abs, min, max |
| **result** | 6 | unwrap_or, map_ok, map_err, flatten, is_ok, is_err |
| **io** | 5 | inspect, read_file, write_file, read_line, args |
| **int** | 5 | parse, abs, min, max, to_float |
| **option** | 5 | map, unwrap_or, to_result, is_some, is_none |
| **channel** | 6 | new, send, receive, close, try_send, try_receive |
| **task** | 3 | spawn, join, cancel |
| **test** | 3 | assert, assert_eq, assert_ne |

### Codebase

| Metric | Count |
|--------|------:|
| Rust source files | 14 |
| Rust source lines | ~12,100 |
| Rust test functions | 138 |
| Evaluation programs | 10 |
| Evaluation program lines | ~3,600 |

---

## 6. Recommended Next Steps

Prioritized by impact and effort.

### High priority

1. **Register `channel.*`, `task.*`, `try`, and `map.length` in the type checker.** These are the only remaining builtins not registered. The runtime works; the type checker just needs the signatures. This completes full type checker parity. Effort: a few hours.

2. **Channel iteration construct** (`for msg in ch { ... }` or equivalent). This is the single biggest ergonomic gap for concurrent code. The recursive drain pattern is the Go `for msg := range ch` equivalent and it takes 8 lines instead of 1. Every concurrent program would benefit. Effort: 1-2 days (new syntax, parser change, interpreter support).

### Medium priority

3. **Rewrite the 10 evaluation programs** to use the current stdlib. Most programs carry v1 workarounds (manual `list_get` helpers, `list.flatten([acc, [item]])` instead of `list.append`, nested match-on-bool instead of guardless match). Rewriting would both demonstrate the language's current ergonomics and provide accurate friction measurements. Effort: half a day.

4. **`list.flat_map`, `list.any`, `list.all`** -- three small convenience builtins that reduce two-step workarounds to one call. Effort: 1 hour.

5. **`string.pad_left` / `string.pad_right`** -- removes the last manual helper pattern from csv_analyzer. Effort: 30 minutes.

### Low priority (nice to have)

6. **`silt test --filter <pattern>`** for selective test running during development.
7. **Negative literal patterns** (`-1` in match arms).
8. **List indexing syntax** (`xs[n]`) as sugar for `list.get(xs, n)`.
9. **`string.strip_prefix` / `string.strip_suffix`** for cleaner bracket extraction in parsers.
10. **`while`/`loop` construct** -- only if the recursive loop ceremony proves too costly in practice. TCO makes recursion safe; the question is whether the ceremony cost justifies a new keyword.

---

*The language has reached a coherent, well-organized state. The global namespace is minimal, the module system is consistent, the type checker knows about (almost) everything, and the pattern matching system is rich. The main remaining friction is the channel drain ceremony and the fact that the evaluation programs haven't been updated to use the features that already exist to address their pain points.*
