---
title: "Why silt"
section: "Guide"
order: 0
description: "How silt compares to Rust, Gleam, Go, and OCaml — what it optimises for and what it deliberately doesn't try to be."
---

# Why silt

silt is a statically-typed, expression-based language for building concurrent
backend services and tools. It aims for the productivity of a high-level
scripting language with the correctness guarantees of an ML-family type
system, and it picks a small set of language features carefully rather than
accumulating them.

Three design commitments shape everything:

1. **Full immutability.** No mutation anywhere. Every value is safe to
   share across tasks.
2. **Pattern matching is the only way to branch.** No `if`, no early
   `return` outside of error flow. The exhaustiveness checker makes sure
   every case is handled.
3. **CSP concurrency with real parallelism.** Lightweight tasks on a
   thread pool, communicating through channels. I/O inside a task
   transparently yields — no `async` / `await` colouring.

## vs. Rust

- You don't manage memory or lifetimes. silt uses a garbage collector.
- You don't write type annotations. Inference is complete for idiomatic
  code.
- You don't get zero-cost abstractions — silt is an interpreter, not a
  native compiler. If you need every cycle, silt is the wrong tool.
- You do get: Result + `?`, exhaustive pattern matching, ADTs, traits,
  typed errors. The error-handling shape is familiar to Rust programmers.

## vs. Go

- Every value is immutable. Data races are not part of the language — a
  task running in parallel can see a shared value without a lock.
- Errors are values of a typed enum, not interface-typed strings. You
  pattern-match on specific variants (`Err(IoNotFound(path)) -> ...`)
  and fall back to `.message()` for the long tail.
- No `nil`. `Option(a)` is the only way to model absence, and the type
  checker makes you handle the `None` case.
- You still get CSP with channels and cheap tasks.

## vs. Gleam

- silt has CSP built on real threads, not the BEAM. Tasks execute on an
  OS thread pool; I/O yields transparently.
- Imports are qualified by default (`list.map(xs, f)`) rather than the
  flat module import style. Large programs stay readable.
- No `actors` as a first-class concept — use channels.

## vs. OCaml / ReScript

- Syntax leans toward C-family (`fn`, braces) rather than ML-style
  (`let … in`). Familiar to most programmers on day one.
- Records are nominal (`type User { name, age }`), not structural. Record
  update is a first-class operator: `user.{ age: 31 }`.
- The standard library is larger and more task-focused (`http`, `json`,
  `postgres`, `channel`, `stream`, `time` all ship in-box).

## When not to use silt

- **Hot numerical loops** — silt is an interpreter. If you are counting
  nanoseconds, use Rust or C.
- **Systems programming** — no `unsafe`, no raw pointers, no inline
  assembly. silt cannot implement its own allocator.
- **Iterative mutation-heavy algorithms** — dynamic programming and
  graph algorithms threaded through `loop` / `fold` are more verbose
  than their mutable counterparts. The trade-off buys concurrency
  safety.

## Where to go next

- **[Getting Started](getting-started.md)** — install and walk through
  the language in under 30 minutes
- **[Language Guide](language-guide.md)** — complete feature reference
- **[Concurrency](concurrency.md)** — the CSP model in depth
