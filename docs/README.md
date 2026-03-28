# Silt Documentation

Silt is a minimal, statically-typed, expression-based language with CSP concurrency. 17 keywords. Fully immutable. Pattern matching as the sole branching construct.

## Guides

- **[Getting Started](getting-started.md)** — Installation, hello world, CLI, your first project
- **[Language Guide](language-guide.md)** — Complete syntax and semantics: types, functions, pattern matching, pipes, error handling, traits, modules
- **[Concurrency](concurrency.md)** — Channels, spawn, select, the cooperative scheduler, common patterns
- **[Standard Library Reference](stdlib-reference.md)** — Every builtin function with signatures and examples

## Deep Dives

- **[Design Decisions](design-decisions.md)** — Architecture, trade-offs, the why behind every major choice

## Quick Reference

```
silt run <file.silt>       -- run a program
silt test [file.silt]      -- run test functions
```

| Aspect         | Choice                                    |
|----------------|-------------------------------------------|
| Keywords       | 17                                        |
| Branching      | `match` only + `when` guard               |
| Types          | HM inference, algebraic + records + traits|
| Mutability     | None (shadowing ok)                       |
| Errors         | `Result`/`Option` + `?` operator          |
| Concurrency    | `spawn`, typed `chan`, `select`            |
| Visibility     | Private default, `pub` to export          |
