---
title: "FFI Guide"
order: 5
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
vm.register_fn1("double", |x: i64| -> i64 { x * 2 });

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
});
```

### Typed registration (auto-marshalling)

The `register_fn0` through `register_fn3` methods handle argument extraction
and type checking automatically:

```rust
vm.register_fn0("answer", || -> i64 { 42 });
vm.register_fn1("double", |x: i64| -> i64 { x * 2 });
vm.register_fn2("add", |a: i64, b: i64| -> i64 { a + b });
vm.register_fn3("clamp", |x: i64, lo: i64, hi: i64| -> i64 {
    x.max(lo).min(hi)
});
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
| `f64` | `Float` | Also accepts `Int` (coerces) |
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
});
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
});
```

From silt:
```silt
let n = parse_int("42")?  -- propagates Err with ?
```

## Higher-Order Functions

Foreign functions work as first-class values. They can be passed to
`list.map`, `list.filter`, piped with `|>`, and stored in data structures:

```rust
vm.register_fn1("square", |x: i64| -> i64 { x * x });
```

```silt
[1, 2, 3] |> list.map(square)   -- [1, 4, 9]
```

## Thread Safety

All registered functions must be `Send + Sync` since they may be called from
spawned task threads. This is enforced by the type system:

```rust
// This works:
vm.register_fn1("pure", |x: i64| -> i64 { x * 2 });

// This won't compile (captures non-Send state):
// let cell = std::cell::RefCell::new(0);
// vm.register_fn0("bad", move || { *cell.borrow() });
```

Use `Arc<Mutex<T>>` if you need shared mutable state in a foreign function.
