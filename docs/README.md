# Silt Documentation

Silt is a minimal, statically-typed, expression-based language with CSP concurrency. 14 keywords. Fully immutable. Pattern matching as the sole branching construct. Only 13 global names.

## Guides

- **[Getting Started](getting-started.md)** -- Installation, hello world, CLI, your first project
- **[Language Guide](language-guide.md)** -- Complete syntax and semantics: types, functions, pattern matching, pipes, error handling, traits, modules
- **[Concurrency](concurrency.md)** -- Channels, tasks, `channel.select`, the cooperative scheduler, common patterns
- **[Standard Library Reference](stdlib-reference.md)** -- Every builtin function with signatures and examples

## Deep Dives

- **[Design Decisions](design-decisions.md)** -- Architecture, trade-offs, the why behind every major choice

## Quick Reference

```
silt run <file.silt>       -- run a program
silt test [file.silt]      -- run test functions
silt repl                  -- interactive REPL
silt fmt <file.silt>       -- format source code
```

| Aspect         | Choice                                                 |
|----------------|--------------------------------------------------------|
| Keywords       | 14: `as else fn import let loop match mod pub return trait type when where` |
| Globals        | 13: `print println panic try Ok Err Some None Stop Continue Message Closed Empty` |
| Branching      | `match` only + `when` guard + guardless match          |
| Types          | HM inference, algebraic + records + traits              |
| Mutability     | None (shadowing ok)                                    |
| Errors         | `Result`/`Option` + `?` operator + `try()` builtin    |
| Concurrency    | `task.spawn`, typed `channel.new`, `channel.select`    |
| Visibility     | Private default, `pub` to export                       |
| Patterns       | Constructor, tuple, list, record, or, range, map, pin (`^`) |
| Collections    | `list.*`, `map.*`, `string.*` (module-qualified)       |
| Tools          | REPL, formatter, test runner                           |
