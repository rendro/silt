---
title: "Standard Library"
section: "Standard Library"
order: 0
---

# Silt Standard Library Reference

Complete API reference for every built-in function in silt.

## Module Index

| Module | Description |
|--------|-------------|
| [Globals](globals.md) | `print`, `println`, `panic`, variant constructors, type descriptors |
| [list](list.md) | Create, transform, query, and iterate over ordered collections |
| [string](string.md) | Split, join, search, transform, and classify strings |
| [map](map.md) | Lookup, insert, merge, and iterate over key-value maps |
| [set](set.md) | Create, combine, query, and iterate over unordered unique collections |
| [int / float](int-float.md) | Parse, convert, and compare integers and floats |
| [result / option](result-option.md) | Transform and query `Result(a, e)` and `Option(a)` values |
| [io / fs / env](io-fs.md) | File I/O, stdin, command-line args, debug inspection, filesystem ops, env vars |
| [test](test.md) | Assertions for test scripts |
| [regex](regex.md) | Match, find, split, replace, and capture with regular expressions |
| [json](json.md) | Parse JSON into typed records/maps, serialize values to JSON |
| [toml](toml.md) | Parse TOML into typed records/maps, serialize values to TOML |
| [math](math.md) | Trigonometry, logarithms, exponentiation, random, and constants |
| [channel / task](channel-task.md) | Bounded channels for concurrent task communication, spawn and join tasks |
| [time](time.md) | Dates, times, instants, durations, formatting, parsing, and arithmetic |
| [http](http.md) | HTTP client and server |
| [bytes](bytes.md) | Immutable byte sequences for binary I/O, hashing, and encoding/decoding |
| [crypto](crypto.md) | SHA-256/512, HMAC, OS CSPRNG, and timing-safe comparison |
| [encoding](encoding.md) | URL / percent encoding per RFC 3986 (base64/hex live in `bytes`) |
| [uuid](uuid.md) | UUID v4 (random) and v7 (time-ordered) generation, parsing, validation |
| [tcp](tcp.md) | Raw TCP listeners and streams with cooperative I/O (optional TLS via `tcp-tls`) |
| [stream](stream.md) | Channel-backed sources, transforms, and sinks with backpressure |
| [postgres](postgres.md) | PostgreSQL pools, queries, transactions, streams, cursors, and LISTEN/NOTIFY (opt-in) |
