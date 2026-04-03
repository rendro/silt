# Silt Documentation

Silt is a statically-typed, expression-based language with 14 keywords, full immutability, and CSP-style concurrency. Pattern matching is the sole branching construct. Types are inferred via Hindley-Milner. Errors are values.

## Documentation

- **[Getting Started](getting-started.md)** — Installation, hello world, language tour with runnable examples
- **[Language Guide](language-guide.md)** — Complete reference: every feature, its design rationale, and trade-offs
- **[Standard Library Reference](stdlib-reference.md)** — Every builtin function with signatures and examples (160+ functions across 17 modules)
- **[Concurrency](concurrency.md)** — CSP model, channels, tasks, select, patterns, the v1 runtime

## Quick Reference

```
silt run <file.silt>       -- run a program
silt test [file.silt]      -- run test functions
silt repl                  -- interactive REPL
silt fmt <file.silt>       -- format source code
```

| Aspect      | Choice |
|-------------|--------|
| Keywords    | 14: `as else fn import let loop match mod pub return trait type when where` |
| Globals     | 13: `print println panic try Ok Err Some None Stop Continue Message Closed Empty` |
| Branching   | `match` only (with/without scrutinee, guards, or-patterns, ranges) |
| Types       | HM inference, ADTs, records, traits with `where` clauses |
| Mutability  | None. Shadowing only. |
| Errors      | `Result`/`Option` values, `?` operator, `when`/`else`, `try()` |
| Concurrency | CSP: `task.spawn`, typed channels, `channel.select` with `^pin` |
| Collections | List `[]`, Map `#{}`, Set `#[]` — all immutable, module-qualified ops |
| Patterns    | Constructor, tuple, list, record, or, range (int + float), map, pin |
| Tools       | REPL, formatter, test runner |
