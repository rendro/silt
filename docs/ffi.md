---
title: "FFI Guide"
section: "Guide"
order: 4
description: "Embed silt in Rust applications. Declare host modules of typed Rust functions, marshal values with FromValue and IntoValue traits."
---

# Foreign Function Interface

Silt can be embedded in Rust applications. The embedder declares **host
modules**: modules whose functions are Rust closures. A silt program
imports a host module like any other module, and every call is
typechecked against the signature the embedder declared.

## Quick Start

```rust
use std::path::Path;
use std::sync::Arc;

use silt::session::{Config, Entry, HostModule, LockPolicy, ProjectSetup, Session};
use silt::{Value, Vm};

// 1. Declare the host module: each function by its silt signature.
let mylib = HostModule::new("mylib").fn1("fn double(x: Int) -> Int", |x: i64| x * 2);

// 2. Open the program in a session that knows the module.
let mut session = Session::new(Config {
    project: ProjectSetup::None,
    lock: LockPolicy::ReadOnly,
    host: vec![mylib],
});
let source = "import mylib\nfn main() { mylib.double(21) }";
let file = session.open_text(Path::new("main.silt"), source);

// 3. Check it. Every static error is in the analysis.
let analysis = session.analyze(file);
for d in &analysis.diagnostics {
    eprintln!("{}", d.message);
}
assert!(!analysis.has_errors());

// 4. Compile it and run it.
let program = session.compile(file, Entry::Main).expect("compiles");
let script = program.functions.into_iter().next().expect("a script");
let result = Vm::new().run(Arc::new(script)).unwrap();
assert_eq!(result, Value::Int(42));
```

## Declaring a host module

`HostModule::new(name)` starts a module. Each function is added with its
signature: a silt `fn` header with no body, which declares the type of
every parameter and the return type. The name in the signature is the
function's name in the module.

```rust
let mylib = HostModule::new("mylib")
    .fn0("fn answer() -> Int", || 42_i64)
    .fn1("fn double(x: Int) -> Int", |x: i64| x * 2)
    .fn2("fn add(a: Int, b: Int) -> Int", |a: i64, b: i64| a + b);
```

From silt:

```silt
import mylib

fn main() {
  println(mylib.add(mylib.double(20), mylib.answer() - 40))
}
```

The module is imported the usual ways: `import mylib`,
`import mylib.{ double }`, `import mylib as m`. A program that does not
import it cannot see it.

### Typed functions

`fn0`, `fn1` and `fn2` convert the arguments from silt values and the
result back, through the `FromValue` and `IntoValue` traits. The Rust
types must match the signature: `Int` is `i64`, `String` is `String`, and
so on (see the table below).

### Functions on values

`function` takes the arguments as a `&[Value]` and returns
`Result<Value, VmError>`. Use it for more arguments, or for a generic
signature:

```rust
let lists = HostModule::new("lists").function(
    "fn first(xs: List(a)) -> Option(a)",
    |args: &[Value]| {
        let Value::List(xs) = &args[0] else {
            return Err(VmError::new("expected a list".into()));
        };
        xs.first().cloned().into_value().map_err(VmError::new)
    },
);
```

The checker has already made sure the arguments have the declared types
and number, so a function can rely on its signature.

## Checking

A host module is checked from its signatures, like a module's `pub fn`s:

- A call with an argument of the wrong type is a type error before
  anything runs (`mylib.double("hello")`: expected Int, got String).
- A call with the wrong number of arguments is an error.
- A function the module does not declare is an error
  (`mylib.triple(3)`: unknown function 'triple' on module 'mylib').

What is wrong with the module itself is reported in every analysis,
whether or not the program imports it, at `<host:mylib>`:

- a signature that is not one `fn` header without a body, or that
  leaves a parameter or the return type untyped;
- a type the signature names that does not exist;
- a host module named like a builtin module (`list`), or two host modules
  with one name.

An `import mylib` in a program whose package also has a `mylib.silt` or a
dependency named `mylib` is an error at the import.

## Supported Types

The `FromValue` and `IntoValue` traits handle conversion between Rust and
silt types:

| Rust type | Silt type | Notes |
|-----------|-----------|-------|
| `i64` | `Int` | |
| `f64` | `Float` | Returning NaN or an infinity raises a runtime error |
| `bool` | `Bool` | |
| `String` | `String` | |
| `()` | `()` | |
| `Value` | any | Passthrough, no conversion |
| `Vec<Value>` | `List(a)` | |
| `Option<T>` | `Option(a)` | Return only |
| `Result<T, String>` | `Result(a, String)` | Return only |

```rust
let users = HostModule::new("users")
    .fn1("fn find(id: Int) -> Option(String)", |id: i64| {
        if id == 1 { Some("alice".to_string()) } else { None }
    })
    .fn1("fn parse_int(s: String) -> Result(Int, String)", |s: String| {
        s.parse::<i64>().map_err(|e| e.to_string())
    });
```

```silt
import users

fn main() {
  match users.find(1) {
    Some(name) -> println("found: {name}")
    None -> println("not found")
  }
}
```

## Functions as values

A host function is a value like any other function: it can be passed to
`list.map`, piped with `|>`, and stored in data structures.

```silt
import list
import mylib

fn main() {
  println([1, 2, 3] |> list.map(mylib.double))   -- [2, 4, 6]
}
```

## Thread Safety

Host functions must be `Send + Sync`, since they may be called from any
thread in the task scheduler's pool. The type system enforces this. Use
`Arc<Mutex<T>>` for shared mutable state in a host function.

## Vm Lifecycle

A `Vm` is a single interpreter instance. `Vm::new().run(script)` runs a
compiled program. The program carries its host functions: the `Vm` needs
no set-up.

**Reusing a Vm.** You can call `vm.run(...)` several times with
different scripts on the same `Vm`. Globals defined by one run persist
into the next. For hermetic runs, build a fresh `Vm::new()` per script.

**Thread safety.** A single `Vm` is **not** `Sync` and must be driven from
one thread (the scheduler owns its own worker threads internally). To run
several scripts in parallel from Rust, create one `Vm` per thread.

## Error Surfacing

- **Static errors** (type errors, unknown names, bad host signatures)
  are the diagnostics of `session.analyze(file)`; `session.compile`
  returns `Err` for a program that has them.
- **Runtime errors** (overflow, out-of-bounds, an `Err` or `None` that
  bubbled to the top) return as `Err(VmError)` from `vm.run`.
- **`panic(...)` in silt code** reaches Rust as an `Err(VmError)` whose
  message carries the panicked string.
- **An `Err` from a host function** becomes a runtime error whose message
  starts with the function's name: `mylib.parse: bad input`.
- **A panic inside a host function** is caught and becomes a runtime
  error (`host function 'mylib.parse' panicked: ...`). The scheduler
  worker survives and other tasks keep running. Returning `Err` is still
  the way to fail; the catch is a safety net.

```rust
match vm.run(script) {
    Ok(value) => println!("result: {:?}", value),
    Err(e) => eprintln!("silt error: {}", e.message),
}
```

Silt code has no access to the host filesystem, network or environment
beyond what the stdlib and your host modules provide.
