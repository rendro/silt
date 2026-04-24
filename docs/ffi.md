---
title: "FFI Guide"
section: "Guide"
order: 4
description: "Embed silt in Rust applications. Register foreign functions, marshal values with FromValue and IntoValue traits."
---

# Foreign Function Interface

Silt can be embedded in Rust applications. The FFI lets you register Rust
functions that are callable from silt code with the same syntax as builtins.

## Quick Start

```rust
use silt::{Vm, Value, VmError};
use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;

let mut vm = Vm::new();

// Register a typed function (auto-marshalling)
vm.register_fn1("double", |x: i64| -> i64 { x * 2 }).unwrap();

// Compile and run silt code
let tokens = Lexer::new("fn main() { double(21) }").tokenize().unwrap();
let program = Parser::new(tokens).parse_program().unwrap();
let mut compiler = Compiler::new();
let functions = compiler.compile_program(&program).unwrap();
let script = std::sync::Arc::new(functions.into_iter().next().unwrap());

let result = vm.run(script).unwrap();
assert_eq!(result, Value::Int(42));
```

## Registration API

### Raw registration

Full control over arguments and return values:

```rust
vm.register_fn("my_func", |args: &[Value]| -> Result<Value, VmError> {
    let Value::Int(n) = &args[0] else {
        return Err(VmError::new("expected Int".into()));
    };
    Ok(Value::Int(n * 2))
}).unwrap();
```

### Typed registration (auto-marshalling)

The `register_fn0` through `register_fn2` methods handle argument extraction
and type checking automatically:

```rust
vm.register_fn0("answer", || -> i64 { 42 }).unwrap();
vm.register_fn1("double", |x: i64| -> i64 { x * 2 }).unwrap();
vm.register_fn2("add", |a: i64, b: i64| -> i64 { a + b }).unwrap();
```

Type mismatches produce clear errors:

```
double: expected Int, got String
```

## Supported Types

The `FromValue` and `IntoValue` traits handle conversion between Rust and
silt types:

| Rust type | Silt type | Notes |
|-----------|-----------|-------|
| `i64` | `Int` | |
| `f64` | `Float` / `ExtFloat` | Also accepts `Int` (coerces) |
| `bool` | `Bool` | |
| `String` | `String` | |
| `()` | `Unit` | |
| `Value` | any | Passthrough, no conversion |
| `Vec<Value>` | `List` | |
| `Option<T>` | `Some(v)` / `None` | Return only |
| `Result<T, String>` | `Ok(v)` / `Err(msg)` | Return only |

## Return Values

### Returning Option

```rust
vm.register_fn1("find_user", |id: i64| -> Option<String> {
    if id == 1 { Some("alice".into()) } else { None }
}).unwrap();
```

From silt:
```silt
match find_user(1) {
  Some(name) -> println("found: {name}")
  None -> println("not found")
}
```

### Returning Result

```rust
vm.register_fn1("parse_int", |s: String| -> Result<i64, String> {
    s.parse::<i64>().map_err(|e| e.to_string())
}).unwrap();
```

From silt:
```silt
let n = parse_int("42")?  -- propagates Err with ?
```

## Higher-Order Functions

Foreign functions work as first-class values. They can be passed to
`list.map`, `list.filter`, piped with `|>`, and stored in data structures:

```rust
vm.register_fn1("square", |x: i64| -> i64 { x * x }).unwrap();
```

```silt
[1, 2, 3] |> list.map(square)   -- [1, 4, 9]
```

## Thread Safety

All registered functions must be `Send + Sync` since they may be called from
any thread in the task scheduler's pool. This is enforced by the type system:

```rust
// This works:
vm.register_fn1("pure", |x: i64| -> i64 { x * 2 }).unwrap();

// This won't compile (captures non-Send state):
// let cell = std::cell::RefCell::new(0);
// vm.register_fn0("bad", move || { *cell.borrow() });
```

Use `Arc<Mutex<T>>` if you need shared mutable state in a foreign function.

## Vm Lifecycle

A `Vm` is a single interpreter instance. The typical embedding pattern is:

```rust
let mut vm = Vm::new();

// 1. Register every foreign function up front.
vm.register_fn1("double", |x: i64| -> i64 { x * 2 })?;
vm.register_fn1("fetch_user", |id: i64| -> Option<String> { ... })?;

// 2. Compile the silt program.
let script = compile("fn main() { ... }")?;

// 3. Run it.
let result = vm.run(script)?;
```

**Register before spawning.** Foreign-function registration mutates the
shared runtime. Once silt code has spawned tasks — or anything else that
clones the runtime `Arc` — `register_fn*` returns an `Err(VmError)`
explaining that the runtime is already shared. Register every foreign
function before the first `vm.run(...)` that might spawn.

**Reusing a Vm.** You can call `vm.run(...)` multiple times with different
scripts on the same `Vm`. Globals defined by one run persist into the next,
which is useful for REPL-style embeddings. If you want hermetic runs,
build a fresh `Vm::new()` per script.

**Thread safety.** A single `Vm` is **not** `Sync` and must be driven from
one thread (the scheduler owns its own worker threads internally, which is
separate from the embedding thread). If you need to run multiple scripts in
parallel from Rust, create one `Vm` per thread.

## Error Surfacing

`vm.run(script)` returns `Result<Value, VmError>`. `VmError` is the single
channel through which every kind of silt runtime failure reaches Rust:

- **Type errors** detected during compilation surface as a `VmError` from
  the compile step, before `run` is called.
- **Runtime errors** (overflow, out-of-bounds, failed `match`, unwrapped
  `Err`/`None` that bubbled to the top) return as `Err(VmError)` from `run`.
- **`panic(...)` in silt code** reaches Rust as an `Err(VmError)` whose
  message carries the panicked string.
- **Panics inside a foreign function** are caught by
  `std::panic::catch_unwind` inside the dispatcher and converted to a
  `VmError`. The scheduler worker survives; other tasks keep running.
  Returning `Err(VmError)` from a foreign function is still strongly
  preferred — panics are a safety net, not an API.

```rust
match vm.run(script) {
    Ok(value) => println!("result: {:?}", value),
    Err(e) => eprintln!("silt error: {}", e.message()),
}
```

Silt code has no access to the host filesystem, network, or environment
beyond what the stdlib (or your registered foreign functions) provides.
Calling an unknown function produces a compile-time error from the type
checker, not a runtime surprise — build the compile pipeline with the
checker in place when embedding untrusted code.
