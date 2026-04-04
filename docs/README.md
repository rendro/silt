# Silt Documentation

Silt is a statically-typed, expression-based language with 14 keywords, full immutability, and CSP-style concurrency with real parallelism. Pattern matching is the sole branching construct. Types are inferred via Hindley-Milner. Errors are values.

## Documentation

- **[Getting Started](getting-started.md)** — Installation, hello world, language tour with runnable examples
- **[Language Guide](language-guide.md)** — Complete reference: every feature, its design rationale, and trade-offs
- **[Standard Library Reference](stdlib-reference.md)** — Every builtin function with signatures and examples (160+ functions across 17 modules)
- **[Concurrency](concurrency.md)** — CSP model, channels, tasks, select, true parallelism
- **[FFI Guide](ffi.md)** — Embed silt in Rust, register foreign functions
- **[Editor Setup](editor-setup.md)** — LSP server, Vim/Neovim configuration

## Quick Reference

```
silt run <file.silt>       -- run a program
silt check <file.silt>     -- type-check without running
silt test [path]           -- run test functions
silt fmt <file.silt>       -- format source code
silt repl                  -- interactive REPL
silt init                  -- create a new main.silt
silt lsp                   -- start the language server
silt disasm <file.silt>    -- show bytecode disassembly
```

| Aspect      | Choice |
|-------------|--------|
| Keywords    | 14: `as else fn import let loop match mod pub return trait type when where` |
| Globals     | 12: `print println panic Ok Err Some None Stop Continue Message Closed Empty` |
| Branching   | `match` only (with/without scrutinee, guards, or-patterns, ranges) |
| Types       | HM inference, ADTs, records, traits with `where` clauses |
| Mutability  | None. Shadowing only. |
| Errors      | `Result`/`Option` values, `?` operator, `when`/`else` |
| Concurrency | CSP with real parallelism: `task.spawn` on OS threads, typed channels, `channel.select` with `^pin` |
| Collections | List `[]`, Map `#{}`, Set `#[]` — all immutable, module-qualified ops |
| Patterns    | Constructor, tuple, list, record, or, range (int + float), map, pin |
| FFI         | Register Rust functions with auto-marshalling via `FromValue`/`IntoValue` |
| Tools       | REPL, formatter, test runner, LSP server, syntax highlighting |
