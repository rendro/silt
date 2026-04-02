# Type Narrowing & Filter-Map Analysis for Silt

You are analyzing a language design problem for **silt**, a statically-typed, expression-based programming language with Hindley-Milner type inference, algebraic data types, pattern matching as the sole branching construct, and no mutation. The language prioritizes minimalism (14 keywords, 13 globals), composability (pipe operator, trailing closures), and safety (Result/Option, no exceptions, no null).

## The Problem

When filtering a list by a predicate that checks a variant, the type system doesn't narrow. This forces a 3-pass anti-pattern with an unreachable panic:

```silt
-- Goal: parse lines, keep successful parses
lines
|> list.map { l -> parse_line(l) }         -- List(Option(Todo))
|> list.filter { opt ->
    match opt { Some(_) -> true; _ -> false }  -- pass 2: keep Somes
  }
|> list.map { opt ->
    match opt { Some(t) -> t; _ -> panic("unreachable") }  -- pass 3: unwrap
  }
```

The `panic("unreachable")` is a code smell forced by the type system — after filtering to only `Some` values, the compiler still sees `Option(Todo)` and demands exhaustive matching.

This surfaces in multiple real-world patterns:
- Parsing fallible data (JSON, CSV, user input) and collecting successes
- Filtering ADT variants: keeping only `KeyValue` lines from a `List(Line)` where `Line = Blank | Comment | Section | KeyValue`
- Processing Result values: collecting `Ok` values from a list of Results

## Your Task

Produce a thorough analysis covering:

### 1. How Other Languages Solve This

For each language below, explain the **mechanism**, show a **concrete code example** of filtering a list of Option/Maybe/nullable to get the inner values, and note **tradeoffs**:

- **Rust** — `filter_map`, `Iterator::flatten` on `Option`, `collect::<Result<Vec<_>, _>>()`
- **Haskell** — `catMaybes`, `mapMaybe`, `rights`, list comprehensions with pattern guards
- **OCaml** — `List.filter_map`, `Seq.filter_map`
- **Scala** — `collect` with partial functions, `flatten` on `Option`, `flatMap`
- **TypeScript** — type guards/predicates (`x is Some<T>`), `filter` with type narrowing
- **Kotlin** — `filterIsInstance`, `mapNotNull`, smart casts
- **Swift** — `compactMap`, `case let` pattern matching in `for` loops
- **Elm** — `List.filterMap`, `Maybe.map` pipelines
- **F#** — `List.choose`, active patterns

For each, note: is it a **library function**, a **type system feature**, or **both**? Does it compose with the pipe/chain idiom?

### 2. Categorize the Approaches

Group the solutions into categories:
- **Library-level**: functions like `filter_map`/`catMaybes` that encode the pattern as a combinator
- **Type-level**: narrowing, refinement types, type guards that change the output type based on a predicate
- **Syntax-level**: pattern-matching in comprehensions, `for case let`, etc.

For each category, analyze: what are the **prerequisites** in the type system? How much **implementation complexity** does it add? How does it interact with **HM inference** specifically?

### 3. Apply to Silt's Design Principles

Silt's design principles (from its docs):
- **Minimal surface area**: 14 keywords, no if/else, pattern matching is the only branching
- **Hindley-Milner inference**: types are rarely annotated, the compiler figures it out
- **Expression-based**: everything is an expression, including `match`, `loop`, blocks
- **Immutable**: no mutation, all bindings are `let`
- **Composable**: pipe operator `|>`, trailing closures, stdlib functions chain naturally
- **Explicit error handling**: Result/Option with `?`, `when`/`else`, `try()` — no exceptions

For each approach category, evaluate:
- Does it fit silt's minimalism? Would it feel native or bolted-on?
- Does it compose with `|>` and trailing closures?
- Does it work with HM inference or fight against it?
- Does it generalize beyond Option (to Result, to user-defined ADTs)?
- What's the implementation cost in silt's current architecture? (The typechecker is in `src/typechecker.rs`, ~5300 lines, with standard HM unification. The interpreter is a tree-walk evaluator.)

### 4. Concrete Proposals for Silt

Propose **2-3 concrete options**, ordered from simplest to most ambitious. For each:

- **Syntax**: show exactly what silt code would look like
- **Semantics**: what types flow through, what the compiler infers
- **Implementation sketch**: what changes in the typechecker, interpreter, and stdlib
- **Limitations**: what it doesn't solve
- **Example**: rewrite the 3-pass anti-pattern above using this approach

### 5. Recommendation

Given silt's principles and current maturity (v1, unreleased, ~15k lines of Rust), what's the right move? Consider:
- What to ship now (immediate value, low risk)
- What to design for later (high value, needs careful design)
- What to explicitly reject (doesn't fit silt's philosophy)

## Context Files

If you need to understand silt's type system and stdlib:
- `docs/getting-started.md` — language tour
- `docs/stdlib-reference.md` — all stdlib functions
- `src/typechecker.rs` — HM typechecker implementation
- `src/interpreter.rs` — runtime, including list module builtins
- `src/value.rs` — Value enum
- `src/ast.rs` — AST types including TypeExpr

## Output Format

Write your analysis as a structured document. Use concrete code examples throughout — abstract descriptions without code are not useful. When comparing languages, use a comparison table where appropriate. The recommendation section should be actionable: "do X now, consider Y later, reject Z because..."
