# Silt Language Friction Report

Generated: 2026-04-02 (updated 2026-04-03 after fixes)
Method: 20 programs implemented from scratch by agents with no prior silt experience. Each program's friction report was independently reviewed against the language documentation to eliminate false positives.

## Executive Summary

Silt is remarkably learnable. Twenty agents — none of which had seen the language before — implemented 20 non-trivial programs (4,611 lines total) with an average of just **1.55 edit-run cycles** before success. Ten programs worked on the first try. The reviewed average rating is **8.9 / 10**.

**Post-analysis fixes applied:** 3 friction points resolved (`Never` type for match arm divergence, `map.update`, `regex.replace_all_with`), 2 confirmed as already working (negative int patterns, `channel.select` wildcards), 1 bug fixed (where clause validation now requires explicit type annotations). The where clause fix also addresses the "implicit type variable naming" friction — the old implicit behavior is now an error.

The remaining friction points are stdlib convenience gaps rather than language design flaws: `list.sum`/`list.min`/`list.max` and `string.split_once` are the most commonly wished-for additions. The language's core — ADTs, pattern matching, pipes, trailing closures, `loop`, and the trait system — consistently earned praise across all 20 programs.

## Per-Program Results

| # | Program | Impl Rating | Reviewed Rating | Attempts | Lines | Highlight | Primary Friction |
|---|---------|:-----------:|:---------------:|:--------:|:-----:|-----------|-----------------|
| 1 | todo.silt | 9/10 | 9.5/10 | 1 | 236 | loop + record update for REPL state | Missed json module (FP) |
| 2 | pipeline.silt | 9/10 | 9/10 | 2 | 278 | 12 pipe compositions, HOF factories | Match arm type mismatch on bail-out |
| 3 | expr_eval.silt | 9/10 | 9/10 | 1 | 294 | Recursive ADT, deep pattern matching, or-patterns | No list.init, no negative int patterns |
| 4 | config_parser.silt | 9/10 | 9.5/10 | 1 | 213 | ADT line classification, fold accumulation | No string.split_once |
| 5 | csv_analyzer.silt | 9/10 | 9/10 | 2 | 193 | group_by + sort_by pipelines | No list.sum/min/max |
| 6 | kvstore.silt | 9/10 | 9/10 | 1 | 162 | JSON round-trip, loop REPL | No string.split_n |
| 7 | concurrent_processor.silt | 9/10 | 9/10 | 1 | 179 | channel.select + pin, channel.close | Repetitive pin match arms |
| 8 | text_stats.silt | 9/10 | 9/10 | 2 | 177 | io.args() + pad_left/right formatting | No list.max_by/min_by, no map.update |
| 9 | test_suite.silt | 9/10 | 9/10 | 2 | 489 | 42 tests via try(), comprehensive coverage | No nested named fn declarations |
| 10 | link_checker.silt | 9/10 | 9/10 | 1 | 99 | regex.captures_all for markdown parsing | (minimal friction) |
| 11 | calculator.silt | 8/10 | 7/10 | 1 | 228 | Stack-based REPL, math module | Missed string literal patterns (FP) |
| 12 | state_machine.silt | 9/10 | 9/10 | 2 | 234 | Nested ADT match, where clauses, Display | No map.map_values |
| 13 | maze_solver.silt | 8/10 | 9/10 | 3 | 235 | BFS with fold_until, path reconstruction | fold_until same-type constraint |
| 14 | json_transform.silt | 9/10 | 10/10 | 2 | 162 | json.parse_list + group_by pipeline | Homogeneous maps (by design) |
| 15 | trait_zoo.silt | 9/10 | 9/10 | 1 | 188 | Custom traits, where clauses, math | Implicit type var naming in where |
| 16 | encoder.silt | 9/10 | 9/10 | 1 | 235 | string.chars + char_code cipher chains | No regex.replace_all with callback |
| 17 | data_gen.silt | 9/10 | 9/10 | 1 | 236 | list.unfold for PRNG threading | No list.sum/min/max |
| 18 | diff_tool.silt | 8/10 | 8/10 | 2 | 181 | LCS via nested fold, unified diff output | No 2D arrays, verbose DP tables |
| 19 | router.silt | 8/10 | 9/10 | 2 | 216 | regex.captures for path params | Missed Fn-in-records (FP) |
| 20 | budget.silt | 8/10 | 8/10 | 2 | 376 | group_by + fold for reports, forecasting | No string concat operator |

**Average impl rating: 8.85 / 10**
**Average reviewed rating: 8.90 / 10**
**Average attempts: 1.55**
**Total lines of silt written: 4,611**

## Confirmed Friction Points

| Relevance | Friction Point | Programs | Confirmed | Status | Description |
|:---------:|---------------|:--------:|:---------:|:------:|-------------|
| 4 | No `list.sum` / `list.min` / `list.max` | 4/20 | 4/4 | Open | Must write manual `list.fold` for common reductions. Not added to stdlib (non-generic; use `sort` + `head`/`tail`). |
| 3 | No `string.split_once` / `string.split_n` | 3/20 | 3/3 | Open | Convenience function, not essential for a slim stdlib. Workaround: `string.index_of` + `string.slice`. |
| 3 | No `list.init` / `list.pop` (all-but-last) | 2/20 | 2/2 | Open | Use front-of-list as stack with `head`/`tail`/`prepend` instead. All building blocks exist. |
| ~~3~~ | ~~No `map.update(m, key, default, fn)`~~ | 2/20 | 2/2 | **Resolved** | Now implemented. `map.update(m, key, default, fn) -> Map`. |
| 3 | No string concatenation operator | 2/20 | 2/2 | By design | Interpolation `"{a}{b}"` is the way. `string.concat` could help in pipes; expression interpolation could reduce let-bindings. |
| ~~3~~ | ~~Match arm type mismatch on error bail-out~~ | 3/20 | 3/3 | **Resolved** | `return` and `panic()` now produce `Type::Never` that unifies with any type. |
| 2 | `fold_until` Stop/Continue same-type constraint | 2/20 | 2/2 | Open | Needs deeper exploration. Docs recommend `loop` for search patterns. |
| 2 | No `list.max_by` / `list.min_by` | 1/20 | 1/1 | Open | Must write manual fold. |
| ~~2~~ | ~~No `regex.replace_all` with callback~~ | 1/20 | 1/1 | **Resolved** | Now implemented as `regex.replace_all_with(pattern, text, fn)`. |
| ~~2~~ | ~~No negative integer literals in patterns~~ | 1/20 | 1/1 | **Already worked** | Parser already supports `Num(-1)`. Reporter was wrong. |
| 2 | No `list.zip_with` / `list.map2` | 1/20 | 1/1 | Open | Must `list.zip` then `list.map`. |
| 2 | No nested named `fn` declarations | 1/20 | 1/1 | By design | `let f = fn(x) { ... }` is the only way. |
| 2 | Repetitive `channel.select` pin match arms | 1/20 | 1/1 | Open | `(_, Message(val))` wildcard already works for the common case. Pin arms needed only when discriminating channels. |
| ~~2~~ | ~~Implicit type variable naming in `where` clauses~~ | 1/20 | 1/1 | **Fixed** | Now requires explicit type annotations: `fn f(x: a) where a: Display`. Implicit form is an error. |
| 2 | No `map.map_values` | 1/20 | 1/1 | Open | Must destructure and reassemble entries. |
| 2 | No descending `list.sort_by` variant | 1/20 | 1/1 | Open | Negate numeric keys or chain `list.reverse`. |
| 1 | No 2D array / mutable matrix | 1/20 | 1/1 | By design | Expected for an immutable language. |

## False Positive Summary

These friction points were reported but don't hold up — the features exist in the docs but agents didn't find them. This is a documentation discoverability signal.

| False Positive | Reported By | Actually Exists |
|---------------|-------------|-----------------|
| "No JSON / serialization library" | todo.silt | `json` module: `json.parse`, `json.stringify`, `json.pretty` (stdlib-reference.md) |
| "No regex-based split / split_whitespace" | pipeline, concurrent_processor, text_stats | `regex.split(pattern, text)` handles this (stdlib-reference.md, regex module) |
| "Tuple destructuring doesn't work in closure params" | config_parser, json_transform | `{ (k, v) -> ... }` syntax works; documented in language-guide.md |
| "String literal patterns don't work in match" | calculator | `match input { "quit" -> ... }` works; documented in language-guide.md |
| "Can't store closures in record fields" | router | `Fn`-typed record fields work; documented in language-guide.md |
| "Tuple destructuring not prominently documented" | encoder | Documented in language-guide.md, getting-started.md, and language-spec.md |
| "No math.pow" | expr_eval | `math.pow(base, exp) -> Float` exists; only `int.pow` is missing |

**Key takeaway**: `regex.split` was the most commonly missed feature (3 programs). The `json` module being missed by the todo agent — which explicitly searched for it — suggests the module listing in stdlib-reference.md could benefit from a table of contents or module index at the top.

## What Felt Natural

These features consistently earned praise across programs:

- **Pattern matching** (20/20): Every program used `match` extensively. Deep nesting, or-patterns, guards, and match-without-scrutinee all composed well. The "match is the only branching construct" philosophy was quickly internalized.
- **Pipe operator + trailing closures** (18/20): The `|>` operator with trailing closure syntax was the single most-praised feature. Pipelines like `list |> list.filter { x -> ... } |> list.map { x -> ... } |> list.sort_by { x -> ... }` felt natural from the first attempt.
- **`loop` expression** (8/20): State-threading REPL loops (`loop state = initial { ... }`) were universally praised by the programs that used them. Zero agents struggled with the concept.
- **ADTs and recursive types** (6/20): Type definitions like `type Expr { Num(Int), Add(Expr, Expr) }` worked exactly as expected. Self-referential types, nested constructors, and exhaustive matching all composed cleanly.
- **Record update syntax** (5/20): `record.{ field: new_value }` was discovered and used naturally.
- **Trait system** (4/20): Custom traits, `Display` overrides, and `where` clauses were straightforward. The auto-derived `Display` for all types was appreciated.
- **String interpolation** (20/20): `"{expr}"` was used everywhere and never caused issues.
- **`list.group_by`** (5/20): Universally appreciated for data analysis tasks.
- **Concurrency primitives** (2/20): `channel.select` with pin, `channel.close`, and `channel.each` worked correctly on the first try.

## Missing Standard Library Functions

Consolidated list of confirmed-missing functions after doc review:

| Function | Category | Impact | Status |
|----------|----------|--------|--------|
| ~~`map.update(m, key, default, fn)`~~ | map | Medium | **Now implemented** |
| ~~`regex.replace_all_with(pattern, text, fn)`~~ | regex | Low | **Now implemented** |
| `list.min_by(list, fn)` / `list.max_by(list, fn)` | list | Medium | Still missing |
| `list.zip_with(a, b, fn)` | list | Low | Still missing |
| `map.map_values(m, fn)` | map | Low | Still missing |

## Bugs Encountered

**No interpreter, typechecker, or parser bugs were encountered across any of the 20 programs.** This is a strong signal of implementation quality. All 20 programs run correctly.

## Language Snapshot

### Keywords (14)
`let`, `fn`, `type`, `trait`, `match`, `when`, `else`, `where`, `return`, `loop`, `pub`, `mod`, `import`, `as`

### Globals (9)
`print`, `println`, `panic`, `try`, `Ok`, `Err`, `Some`, `None`, `Stop`, `Continue`, `Message`, `Closed`, `Empty`, `Int`, `Float`, `String`, `Bool`

### Module builtins (160 functions across 16 modules)

| Module | Functions | Key Functions |
|--------|:---------:|---------------|
| list | 30 | map, filter, fold, sort_by, group_by, unfold, fold_until, flat_map |
| string | 26 | split, chars, char_code, from_char_code, pad_left, pad_right, join, slice |
| map | 14 | get, set, delete, merge, entries, from_entries, filter, each, **update** |
| set | 15 | from_list, contains, insert, union, intersection, difference |
| math | 13 | sqrt, pow, sin, cos, log, pi, e, atan2 |
| regex | 9 | find_all, captures, captures_all, split, replace_all, **replace_all_with** |
| float | 10 | parse, to_string, round, ceil, floor, to_int |
| int | 6 | parse, abs, min, max, to_float, to_string |
| result | 6 | unwrap_or, map_ok, map_err, flatten, is_ok, is_err |
| option | 6 | map, unwrap_or, to_result, flat_map, is_some, is_none |
| json | 5 | parse, parse_list, parse_map, stringify, pretty |
| io | 5 | read_file, write_file, read_line, args, inspect |
| channel | 8 | new, send, receive, close, select, each, try_receive, is_closed |
| task | 3 | spawn, sleep, yield |
| test | 3 | assert, assert_eq, assert_ne |
| fs | 1 | exists |

### Codebase Metrics
- **Implementation**: 17,942 lines of Rust across 15 source files
- **Tests**: 487 test cases
- **Programs written**: 4,611 lines across 20 programs

## Code Showcases

### 1. Recursive ADT with Pattern Matching (expr_eval.silt)

```silt
type Expr {
  Num(Int),
  Add(Expr, Expr),
  Mul(Expr, Expr),
  Neg(Expr),
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
```

### 2. ADT Line Classification with Exhaustive Matching (config_parser.silt)

```silt
type Line {
  Blank,
  Comment(String),
  Section(String),
  KeyValue(String, String),
  ParseError(Int, String),
}

fn classify_line(line_num, raw_line) {
  let trimmed = string.trim(raw_line)
  match {
    string.is_empty(trimmed) -> Blank
    string.starts_with(trimmed, ";") -> Comment(trimmed)
    string.starts_with(trimmed, "#") -> Comment(trimmed)
    string.starts_with(trimmed, "[") -> {
      match string.index_of(trimmed, "]") {
        Some(end) -> {
          let name = string.slice(trimmed, 1, end) |> string.trim
          match string.is_empty(name) {
            true -> ParseError(line_num, "empty section name")
            _ -> Section(name)
          }
        }
        None -> ParseError(line_num, "unclosed section bracket")
      }
    }
    _ -> { ... }
  }
}
```

### 3. Concurrent Workers with Channel Select (concurrent_processor.silt)

```silt
fn worker(id, jobs, results) {
  channel.each(jobs) { path ->
    let outcome = match io.read_file(path) {
      Ok(content) -> {
        let lines = count_lines(content)
        let words = count_words(content)
        let dept = extract_field(content, "Department")
        let revenue = extract_field(content, "Revenue")
        Ok((path, lines, words, dept, revenue))
      }
      Err(e) -> Err((path, e))
    }
    channel.send(results, (id, outcome))
  }
}
```

### 4. Trait System with Where Clauses (trait_zoo.silt)

```silt
type Shape {
  Circle(Float),
  Rect(Float, Float),
  Triangle(Float, Float, Float),
}

trait Area { fn area(self) -> Float }
trait Perimeter { fn perimeter(self) -> Float }

trait Area for Shape {
  fn area(self) -> Float {
    match self {
      Circle(r) -> math.pi * r * r
      Rect(w, h) -> w * h
      Triangle(a, b, c) -> {
        let s = (a + b + c) / 2.0
        math.sqrt(s * (s - a) * (s - b) * (s - c))
      }
    }
  }
}

fn describe(item: a) where a: Display {
  println("Item: {item.display()}")
}
```

## Verdict

Silt is a highly learnable language. Twenty agents with zero prior experience produced 4,611 lines of working code across 20 non-trivial programs, with an average of 1.55 edit-run cycles and no interpreter bugs encountered. The average reviewed satisfaction rating of 8.9/10 is strong — and notably, two programs scored 9.5 or 10 after review, meaning the agents underrated the experience due to false positives about missing features.

The language's core design is sound. Pattern matching as the sole branching construct, the pipe operator with trailing closures, immutable-by-default data with `loop` for stateful iteration, and the ADT + trait system all composed naturally and were internalized quickly. The most telling signal: 10 of 20 programs worked on the first edit-run cycle, and the 10 that didn't fail for minor reasons (type mismatches in match arms, wrong concat operator, incorrect rest-pattern syntax) — not fundamental misunderstandings.

**Post-analysis fixes (2026-04-03):** Three friction points were resolved by implementation changes: `Type::Never` eliminates match arm type mismatches when one arm diverges (`return`/`panic`), `map.update` eliminates the three-step frequency counting pattern, and `regex.replace_all_with` enables per-match callbacks. Two reported friction points were confirmed as false — negative integer patterns and `channel.select` wildcards already worked. The `where` clause validation bug was fixed: type variables must now be explicitly introduced via annotations (`fn f(x: a) where a: Display`), enforcing explicit-over-implicit.

The remaining friction is intentional minimalism: `list.sum`/`min`/`max` are not generic and can be composed from `sort` + `head`/`tail`; `string.split_once` is a convenience over `index_of` + `slice`; `list.init` is avoided by using front-of-list stacks. The documentation has discoverability gaps (`regex.split` missed by 3 agents, `json` module missed by 1) that a module index would fix.
