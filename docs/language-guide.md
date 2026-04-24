---
title: "Language Guide"
section: "Guide"
order: 2
description: "A complete reference to silt's features: pattern matching, types, traits, closures, concurrency, and error handling."
---

# Silt Language Guide

Silt is a statically-typed, expression-based programming language with full
immutability, pattern matching as the sole branching construct, and CSP-style
concurrency. File extension: `.silt`.

This guide is split into focused pages:

## Language Features

- [Bindings and Functions](language/bindings-and-functions.md) -- `let`, `fn`, closures, trailing closures, early return
- [Types](language/types.md) -- primitives, enums, generics, records, tuples, recursive types, type ascription
- [Generics](language/generics.md) -- type parameters, constraints, higher-kinded types, bounded polymorphism
- [Pattern Matching](language/pattern-matching.md) -- match, literals, constructors, tuples, records, lists, maps, or-patterns, guards, ranges, pin, when/else, exhaustiveness
- [Error Handling](language/error-handling.md) -- Result, Option, `?` operator, when let-else, Never type
- [Collections](language/collections.md) -- lists, maps, sets
- [Loops, Pipes, and Other Features](language/loops-and-pipes.md) -- pipe operator, string interpolation, loop expression, ranges, comments
- [Operators and Precedence](language/operators.md) -- full operator table, precedence, associativity, newline rules
- [Traits](language/traits.md) -- declaration, implementation, Self type, built-in traits, where clauses
- [Modules](language/modules.md) -- file-based modules, imports, built-in modules, standard library
- [Testing](language/testing.md) -- test runner, assertions, file and function conventions

## Design

- [Design Decisions](language/design-decisions.md) -- trade-offs and rationale behind language choices

## Other Guides

- [Concurrency](concurrency.md) -- CSP model, tasks, channels, select, fan-out patterns
