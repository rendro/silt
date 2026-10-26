---
title: "Error Codes"
section: "Language"
order: 14
description: "What each diagnostic code of silt means: the errors E0001 to E0801 and the warnings W0201 to W0203."
---

# Error Codes

Every diagnostic silt reports has a code. The terminal shows its kind
(`error[type]`, `warning[type]`); `silt check --format json` and the
language server give the code itself: `E` and four digits for an error,
`W` and four digits for a warning. Its hundreds say when the problem
was found: 0 lexing, 1 parsing, 2 names, 3 types, 4 compiling, 5 the
entry point, 6 packages, 7 running, 8 formatting. A code is always an
error or always a warning, and keeps its meaning; numbers that are
missing belong to codes that are gone.

## Lexing (0xx)

| Code | Meaning |
|------|---------|
| `E0001` | A character that starts no token. |
| `E0002` | A string that is not closed before the end of the file. |
| `E0003` | A `{- -}` comment that is not closed before the end of the file. |
| `E0004` | A backslash in a string that starts no escape silt has. |
| `E0005` | A number literal that is not well-formed, or out of its type's range. |

## Parsing (1xx)

| Code | Meaning |
|------|---------|
| `E0101` | A token other than the one the grammar needs here. |
| `E0102` | A name is needed here. |
| `E0103` | A declaration (`fn`, `type`, `trait`, `let`, `import`) is needed here. |
| `E0104` | An expression is needed here. |
| `E0105` | A pattern is needed here. |
| `E0106` | A type is needed here. |
| `E0107` | A list, block or call that is not closed where it ends. |
| `E0108` | Two declarations, or two statements, on one line. |
| `E0109` | Syntax of another language, or of an older silt, that silt does not have. |
| `E0110` | A name bound twice at the top level of a file. |
| `E0111` | A field named twice in an anonymous record type or literal. |
| `E0112` | Expressions, patterns or types nested deeper than the parser follows. |
| `E0113` | A declaration whose parts are not allowed together (a supertrait on an impl, an annotated `type` parameter, a lowercase type name, `opaque` on an alias or on a private type, ...). |
| `E0114` | The count of the lex and parse errors of a file that are not shown, at the first of them. |

## Names (2xx)

| Code | Meaning |
|------|---------|
| `E0201` | A name that resolves to nothing where it is used. |
| `E0202` | A bare variant name that variants of several enums share. |
| `E0203` | A member a module does not have: `m.nope`, `import m.{ nope }`, `m.Nope` as a type, trait or variant. |
| `E0204` | A name of another module that is not `pub` there. |
| `E0205` | A name in a closure that is both the closure, which its `let` binds to the name, and what the name meant before that `let`. |

## Types (3xx)

| Code | Meaning |
|------|---------|
| `E0301` | A value of one type where another type is needed. |
| `E0302` | A name in a pin pattern (`^x`) that nothing binds. |
| `E0303` | A constructor that no enum declares, or a record literal written through the name of an anonymous record type. |
| `E0304` | A record literal or pattern whose type name names no record type. |
| `E0305` | A field or method that the type at hand does not have: `p.b` on a record without `b`, a field a record literal or update gives that its type does not declare. |
| `E0306` | A field that the record at hand does not declare. |
| `E0307` | A record literal without a field its type needs. |
| `E0308` | A method that the value's type does not have. |
| `E0309` | A method called in a way it cannot be (a method without `self` on a value). |
| `E0310` | A method several traits provide. |
| `E0311` | A variant that the enum does not declare. |
| `E0312` | A type name in an annotation that names no type. |
| `E0313` | A trait name that names no trait. |
| `E0314` | A type has no impl of a trait it is required to have. |
| `E0315` | A type cannot have the derived `Equal`, `Compare` or `Hash` it is used with: one of its fields has none. (No diagnostic has this code today.) |
| `E0316` | A call needs a trait bound the enclosing function does not declare. |
| `E0317` | The wrong number of arguments, fields, bindings or type arguments. |
| `E0318` | A type that would have to contain itself. |
| `E0319` | A type that cannot be determined. |
| `E0320` | A `match` that does not cover every value. |
| `E0321` | In a `match` without a scrutinee, a `_` arm before other arms, or a second `_` arm: the arms after it never run. |
| `E0322` | A pattern that cannot stand where it is (a refutable `let`, an or-pattern whose alternatives bind different names, ...). |
| `E0323` | A name bound twice in one pattern. |
| `E0324` | `?` where it cannot propagate. |
| `E0325` | `loop(...)` or a `when` else body where it may not stand. |
| `E0326` | An operation the operand types do not support. |
| `E0328` | A use of a builtin module that is not imported. |
| `E0330` | A trait declaration that breaks a trait rule. |
| `E0331` | An impl that breaks a trait rule: a missing or undeclared method, a hand-written impl of a derived trait, ... |
| `E0332` | An impl written in a module that declares neither its trait nor its type. |
| `E0333` | Two declarations of one name. |
| `E0334` | A record, enum or alias declaration that is not well-formed. |
| `E0335` | A type annotation or bound that is not well-formed. |
| `E0336` | A name that hides another one where both are needed. |
| `E0338` | A field named twice in a record type, literal, pattern or update. |
| `E0339` | A top-level `let` whose initialiser can reach the `let` itself. |
| `E0340` | A statement whose value nothing uses. |
| `E0342` | A type given to a decoder that has no decoder for it (a field of a type no decoder exists for, an enum, a generic type, ...), or an opaque type decoded or json-encoded outside its module without the module's impl. |
| `E0343` | A type constructor where a type is needed, or a type where a type constructor is needed. |
| `E0344` | A value of an opaque type built, taken apart, updated, read, spread or passed into an open row outside its module. |

## Compiling and the entry point (4xx, 5xx)

| Code | Meaning |
|------|---------|
| `E0401` | An import of a module that is not there. |
| `E0402` | Modules that import each other in a cycle. |
| `E0405` | Something the bytecode cannot express: too many constants, locals, arguments, a jump too far. |
| `E0409` | A defect in silt: the compiler lost track of its own state. |
| `E0410` | A module file named like a builtin module: `import` of the name always means the builtin one. |
| `E0411` | A host module an embedder declared that a builtin module, a dependency or a module file of the same name hides or is hidden by. |
| `E0412` | A host function whose signature is not one `fn` header. |
| `E0501` | `silt run` of a program without a `main` function. |
| `E0502` | A `main` that takes parameters. |
| `E0503` | A test function that cannot be called with no arguments. |

## Packages (6xx)

| Code | Meaning |
|------|---------|
| `E0601` | A `silt.toml` that is not well-formed. |
| `E0602` | A path dependency whose directory is not there. |
| `E0603` | A dependency whose directory holds no `silt.toml`. |
| `E0604` | Two different package sources with one `[package].name`. |
| `E0605` | A dependency key equal to a module of the same package. |
| `E0606` | A git dependency that cannot be resolved or fetched. |
| `E0607` | A `silt.lock` that is not well-formed. |
| `E0608` | A `silt.lock` entry that is not what the manifest resolves to, where the lockfile may not be rewritten. |
| `E0609` | A symbolic link in a dependency's sources. |
| `E0610` | A package file that cannot be read or written. |
| `E0611` | A git dependency's checkout whose checksum is not the one `silt.lock` pins. |

## Running (7xx)

| Code | Meaning |
|------|---------|
| `E0701` | A runtime error: a panic, a division by zero, an out-of-range conversion, ... |
| `E0702` | `main` returned `Err(..)`. |
| `E0703` | Tasks failed and nobody joined them. |
| `E0704` | An instruction met a value of a kind it cannot work on: the typechecker let through a program it should have rejected. |

## Formatting (8xx)

| Code | Meaning |
|------|---------|
| `E0801` | `silt fmt` could not follow the tokens of a file that parses, or its result failed its own check: a defect of the formatter. The text is left as it is. |

## Warnings (Wxxx)

A warning never stops `silt run`, `silt test` or the REPL;
`silt check --deny-warnings` exits with 1 when there is one. Its help
line names the fix, which is the one way to silence it.

| Code | Meaning |
|------|---------|
| `W0201` | A local that hides an imported module, an alias or an imported name. |
| `W0202` | An import that nothing in the file uses. |
| `W0203` | A `let` name or a pattern name that nothing reads. |
