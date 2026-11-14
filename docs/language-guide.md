---
title: "Language Guide"
section: "Guide"
order: 2
description: "A complete reference to silt's features: pattern matching, types, traits, closures, concurrency, and error handling."
---

# Silt Language Guide

Silt is a statically-typed, expression-based programming language with full
immutability, pattern matching as the way to branch, and CSP-style
concurrency. File extension: `.silt`.

This guide is split into focused pages:

## Language Features

- [Bindings and Functions](language/bindings-and-functions.md) -- `let`, `fn`, closures, trailing closures, early return
- [Types](language/types.md) -- primitives, enums, generics, records, tuples, recursive types, type ascription
- [Generics](language/generics.md) -- type parameters, constraints, bounded polymorphism, type-directed dispatch
- [Pattern Matching](language/pattern-matching.md) -- match, literals, constructors, tuples, records, lists, maps, or-patterns, guards, ranges, pin, when/else, exhaustiveness
- [Error Handling](language/error-handling.md) -- Result, Option, `?` operator, when let-else, Never type
- [Collections](language/collections.md) -- lists, maps, sets
- [Loops, Pipes, and Other Features](language/loops-and-pipes.md) -- pipe operator, string interpolation, loop expression, ranges, comments
- [Operators and Precedence](language/operators.md) -- full operator table, precedence, associativity, newline rules
- [Traits](language/traits.md) -- declaration, implementation, Self type, built-in traits, where clauses
- [Functors](language/functors.md) -- traits of type constructors: Functor, Applicative, Monad, traverse and sequence, the laws
- [Modules](language/modules.md) -- file-based modules, imports, built-in modules, standard library
- [Testing](language/testing.md) -- test runner, assertions, file and function conventions
- [Error Codes](language/error-codes.md) -- what each diagnostic code (`E0301`, ...) means

## Design

- [Design Decisions](language/design-decisions.md) -- trade-offs and rationale behind language choices

## Other Guides

- [Language card](language-card.md) -- the syntax in brief and every builtin signature, on one page
- [Concurrency](concurrency.md) -- CSP model, tasks, channels, select, fan-out patterns
- [Why silt](why-silt.md) -- how silt compares to Rust, Go, Gleam and OCaml
- [Editor Setup](editor-setup.md) -- the language server, VS Code, Neovim and Vim
- [FFI](ffi.md) -- embedding silt in a Rust program, and Rust functions for silt

## Standard Library

Each built-in module has a reference page: its types, a table of its
functions, and a section for each function with its signature and an
example. An editor shows the same text when you hover a name.

- [Globals](stdlib/globals.md) -- the prelude: `print`, `println`, `panic`, `Option`, `Result`, the types that need no import
- [Errors](stdlib/errors.md) -- the error enum of each module, and matching on its variants
- [list](stdlib/list.md), [map](stdlib/map.md), [set](stdlib/set.md) -- the collections
- [string](stdlib/string.md) -- inspecting, slicing, splitting and building strings
- [int / float](stdlib/int-float.md) -- parsing, converting and the limits of numbers
- [math](stdlib/math.md) -- roots, powers, logarithms, trigonometry, constants
- [result / option](stdlib/result-option.md) -- combinators of `Result` and `Option`
- [bytes](stdlib/bytes.md) -- byte strings, hex and base64
- [encoding](stdlib/encoding.md) -- URL and form encoding
- [crypto](stdlib/crypto.md) -- hashes, HMAC, random bytes
- [uuid](stdlib/uuid.md) -- making, parsing and checking UUIDs
- [regex](stdlib/regex.md) -- regular expressions
- [json](stdlib/json.md), [toml](stdlib/toml.md), [csv](stdlib/csv.md) -- reading and writing text formats into typed values
- [time](stdlib/time.md) -- instants, dates, durations, formatting and parsing
- [io / fs / env](stdlib/io-fs.md) -- files, stdin and stderr, the file system, environment variables
- [process](stdlib/process.md) -- the exit code of the program
- [channel / task](stdlib/channel-task.md) -- channels and tasks
- [stream](stdlib/stream.md) -- sources, transforms and sinks over channels
- [http](stdlib/http.md) -- an HTTP client and server
- [tcp](stdlib/tcp.md) -- TCP listeners and connections, TLS
- [postgres](stdlib/postgres.md) -- a PostgreSQL client with a pool, transactions, cursors and notifications
- [test](stdlib/test.md) -- the assertions of `silt test`
