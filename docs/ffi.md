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

use silt::session::{Config, Entry, HostModule, LockPolicy, ProjectSetup, Session};
use silt::{Buffer, HostIo, Value, Vm};

// 1. Declare the host module: each function by its silt signature.
let mylib = HostModule::new("mylib").fn1("fn double(x: Int) -> Int", |x: i64| x * 2);

// 2. Open the program in a session that knows the module.
let mut session = Session::new(Config {
    project: ProjectSetup::None,
    lock: LockPolicy::ReadOnly,
    host: vec![mylib],
});
let source = "import mylib\nfn main() { mylib.double(21) }";
let file = session.set_overlay(Path::new("main.silt"), source.to_string());

// 3. Check it. Every static error is in the analysis.
let analysis = session.analyze(file);
for d in &analysis.diagnostics {
    eprintln!("{}", d.message);
}
assert!(!analysis.has_errors());

// 4. Compile it and run it. What the program prints is collected in `out`.
let program = session.compile(file, Entry::Main).expect("compiles");
let out = Buffer::new();
let result = Vm::new(HostIo::buffer(&out)).run_program(&program).unwrap();
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

From silt (a program run in a session that declares `mylib`):

```silt
import mylib

println(mylib.add(mylib.double(20), mylib.answer() - 40)) -- 42
```

The module is imported the usual ways: `import mylib`,
`import mylib.{ double }`, `import mylib as m`. A program that does not
import it cannot see it.

### Typed functions

`fn0`, `fn1` and `fn2` convert the arguments from silt values and the
result back, through the `FromValue` and `IntoValue` traits. The Rust
types must match the signature: `Int` is `i64`, `String` is `String`, and
so on (see the table below). The signature must declare as many
parameters as the closure takes: `.fn1("fn answer() -> Int", ..)` is an
error.

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
and number, so a function can rely on its signature. The result is
checked against the signature's return type when the function returns:
a `-> Int` function that returns a `String` is a runtime error that
names it (`mylib.count: its signature returns Int, but it returned
String "three"`).

A host function cannot take or return a function (`fn f(g: Fn(Int) ->
Int) -> Int` is an error): a host function has no way to call one.

## Checking

A host module is checked from its signatures, like a module's `pub fn`s:

- A call with an argument of the wrong type is a type error before
  anything runs (`mylib.double("hello")`: expected Int, got String).
- A call with the wrong number of arguments is an error.
- A function the module does not declare is an error
  (`mylib.triple(3)`: module 'mylib' has no member 'triple').

What is wrong with the module itself is reported in every analysis,
whether or not the program imports it, at `<host:mylib>`:

- a signature that is not one `fn` header without a body, or that
  leaves a parameter or the return type untyped;
- a signature that takes or returns a function;
- a typed function (`fn0`, `fn1`, `fn2`) whose signature declares another
  number of parameters;
- a type the signature names that does not exist;
- a host module whose name is not an identifier (`my-lib`), one named
  like a builtin module (`list`), or two host modules with one name.

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

match users.find(1) {
  Some(name) -> println("found: {name}")
  None -> println("not found")
}
let n = users.parse_int("42")?   -- propagates Err with ?
```

## Functions as values

A host function is a value like any other function: it can be passed to
`list.map`, piped with `|>`, and stored in data structures.

```silt
import list
import mylib

println([1, 2, 3] |> list.map(mylib.double)) -- [2, 4, 6]
```

## Thread Safety

Host functions must be `Send + Sync`, since they may be called from any
thread in the task scheduler's pool. The type system enforces this. Use
`Arc<Mutex<T>>` for shared mutable state in a host function.

## Output and clock

A `Vm` is made with a `HostIo`: where the program's output goes, and the
clock it reads. It is set once, when the `Vm` is made, and holds for the
program's own code and for every task.

```rust
let out = Buffer::new();
let vm = Vm::new(HostIo::buffer(&out));
```

| Constructor | stdout and stderr | Clock |
|-------------|-------------------|-------|
| `HostIo::buffer(&buffer)` | both into `buffer`, in memory | system |
| `HostIo::new(stdout, stderr)` | any two `Output`s | system |
| `HostIo::process()` | the process's stdout and stderr (what the `silt` command uses) | system |
| `.clock(clock)` on any of them | unchanged | `clock` |

**Output.** `print` and `println` write to stdout. What the runtime
prints for the program goes to stderr: the report of a task that failed
and that nobody joined, and the log of an `http.serve` handler that
failed. A `Buffer` collects text in memory; its clones share the text,
so the embedder keeps one and reads it with `contents()` or `take()`.
For anything else, implement `Output`:

```rust
struct Lines(std::sync::mpsc::Sender<String>);

impl Output for Lines {
    fn write(&self, text: &str) -> std::io::Result<()> {
        self.0.send(text.to_string()).map_err(std::io::Error::other)
    }
}
```

`write` is called once for each `print`, `println` or report, from the
thread that runs the program and from the scheduler's threads. An error
it returns for a `print` or `println` is a runtime error of the program
(`cannot write to stdout: ...`), and so is a panic inside it; for a
report on stderr both are dropped.

The report of tasks that failed and that nobody joined is written when
`run_program` returns, for the tasks that have failed by then. It does
not change the result: `run_program` still returns `main`'s value. A
task that fails later is reported when the `Vm` is dropped, if it has
failed by then; `vm.settle()` waits for every task first (see "The end
of a program" below). With `HostIo::process()`, a program
that writes to a closed stdout pipe ends the process quietly with status
141; give `HostIo::new` your own `Output` if the process must go on.

**Clock.** A `Clock` gives the time of day, a monotonic reading, and a
way to block:

```rust
pub trait Clock: Send + Sync {
    /// The time since the Unix epoch.
    fn now(&self) -> Duration;
    /// The time since some fixed moment; it never goes back.
    fn monotonic(&self) -> Duration;
    /// Block the calling thread until `duration` has passed.
    fn sleep(&self, duration: Duration);
}
```

| The program | reads |
|-------------|-------|
| `time.now`, `time.today`, the timestamp in `uuid.v7`, the seed of `math.random` (at its first call) | `now` |
| `time.sleep` outside a task | `sleep`, for no longer than a `task.deadline` around it allows |
| `time.sleep` in a task, `channel.timeout`, `channel.recv_timeout` | `monotonic`: the wait ends when the reading reaches its deadline |
| `task.deadline`, `task.spawn_until`, `SILT_IO_TIMEOUT` | `monotonic`: I/O started after the deadline fails at once, and a wait for I/O ends when the reading reaches the deadline |

The runtime's own threads wait in real time between two readings of an
embedder's clock. A task's sleep, the channel timeouts and a deadline
that ends a task's wait for I/O end within a millisecond of the moment
the clock reaches the deadline. Everything that
is not the program's own waiting stays in real time: the scheduler's
time slices, the socket timeouts of `http` and `tcp`, and how long I/O
takes.

If a method of the clock panics, the clock is not called again and the
program ends with the runtime error `the clock panicked: ...`: the
builtin that read it fails, and every wait that was pending on the clock
ends, its waiter failing the same way at its next step.

`time.today` gives the date of `now` in the local time zone (in UTC in a
build without the `local-clock` feature).

A program with a clock of its own, whose output is collected:

```rust
use std::path::Path;
use std::time::Duration;

use silt::session::{Config, Entry, LockPolicy, ProjectSetup, Session};
use silt::{Buffer, Clock, HostIo, Vm};

// A clock that starts at a fixed time and moves only when the
// program sleeps.
struct Simulated(std::sync::Mutex<Duration>);

impl Clock for Simulated {
    fn now(&self) -> Duration {
        // 2026-10-05T12:00:00Z, plus what has passed.
        Duration::from_secs(1_791_201_600) + self.monotonic()
    }
    fn monotonic(&self) -> Duration {
        *self.0.lock().unwrap()
    }
    fn sleep(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}

let mut session = Session::new(Config {
    project: ProjectSetup::None,
    lock: LockPolicy::ReadOnly,
    host: vec![],
});
let source = r#"
import time
fn main() {
  println("started")
  time.sleep(time.minutes(90))
  println(time.now() |> time.to_utc |> time.format("%H:%M"))
}
"#;
let file = session.set_overlay(Path::new("main.silt"), source.to_string());
let program = session.compile(file, Entry::Main).expect("compiles");

// The program's output is collected in `out`; it reads `Simulated`.
let out = Buffer::new();
let io = HostIo::buffer(&out).clock(Simulated(Default::default()));
Vm::new(io).run_program(&program).unwrap();
assert_eq!(out.contents(), "started\n13:30\n");
```

### WebAssembly

The library builds for `wasm32-unknown-unknown` with
`default-features = false`. That target has no clock, no threads and no
source of randomness of its own, so the embedder supplies them:

- **Clock.** Give the `Vm` a `Clock` (in a browser, from `Date.now()`
  and `performance.now()`): the system clock panics there. `sleep`
  cannot block a browser's main thread; it can return at once, or move
  a simulated clock on as above.
- **Tasks.** There are no worker threads: the thread that runs the
  program runs its tasks, whenever the program waits (a receive, a
  send, a join, a sleep). A task that nobody ever waits for does not
  run.
- **Timers and I/O.** The same thread fires the timers while it waits,
  and an I/O operation runs on it.
- **Randomness.** `getrandom` and `uuid` refuse to build for the target
  until the embedder's crate picks a source: in a browser, add
  `getrandom = { version = "0.2", features = ["js"] }` and
  `uuid = { version = "1", features = ["js"] }` to its dependencies.

## Vm Lifecycle

A `Vm` is a single interpreter instance. `vm.run_program(&program)` runs
a compiled program: it takes in the types of the program's values and
its global slots (one per top-level function, `let`, host function and
trait method), then runs the program's script and returns `main`'s
value. The program carries its host functions, and the `Vm` its output
and clock: there is no other set-up.

**The end of a program.** `run_program` returns when `main` has
returned; tasks that the program spawned may still be at work.
`vm.settle()` waits until the program has ended as `silt run` defines
it: until none of its tasks can do more (each has ended or waits, and
no timer and no I/O is pending). It then drops the tasks that still
wait, and reports on stderr the tasks that failed and that nobody
joined. An embedder that wants the exit rule of `silt run` calls it
after a `run_program` that returned `Ok`.

Without `settle`, the program's tasks go on for as long as the `Vm`
lives: a task that waits stays where it is, and a task that fails is
reported at the return of the next call into the `Vm`, or when the
`Vm` is dropped, if it has failed by then; a task that has not failed
by then leaves no report. An embedder that must not wait for a
program's background tasks (a playground with a time limit) does not
call `settle`, and drops the `Vm`.

**Dropping a Vm** ends its program where it is. The threads that
served it end (the scheduler's workers, the timer thread, the I/O workers; each when what
it is doing returns), tasks that are still running or waiting never run
again, and pending timers never fire. The state behind `math.random`
and `uuid.v7` belongs to the `Vm` too: one `Vm` does not affect
another's numbers.

**Reusing a Vm.** Run one program per `Vm`: build a fresh `Vm::new(io)`
for each. (The entries of a REPL session are compiled to follow one
another, and share one `Vm`.)

**Thread safety.** A single `Vm` is **not** `Sync` and must be driven from
one thread (the scheduler owns its own worker threads internally). To run
several scripts in parallel from Rust, create one `Vm` per thread.

## Limits

**Time slice.** `vm.set_time_slice(steps)` makes the program run in
slices of that many steps: a task gives way to the other tasks after
`steps` instructions (the scheduler's own slice is 2000), and the thread
that runs the program's own code stops and goes on after `steps` too,
where it otherwise runs `main` without a break. What a program computes
does not depend on the slice; a host sets one to test exactly that. With
`set_time_slice(1)` the program is stopped and resumed after every
instruction and after every step of a builtin that calls back into it.

**Step budget.** `vm.set_step_budget(steps)` lets the program run that
many more steps, those of its tasks included. When they are used up,
whatever of the program still runs ends with a `VmError` whose
`out_of_steps` is `true` (message `the step budget is used up`): `main`,
and each task, whose join gives the error on. A program that loops for
ever ends there. Steps are counted at the end of each slice that ran its
full length, so the program may run up to one slice per thread more
than its budget; with `set_time_slice(1)` the count is exact. A wait is
no step: a program that waits for something that never comes is ended by
the deadlock check or by dropping the `Vm`, not by its budget.

## Error Surfacing

- **Static errors** (type errors, unknown names, bad host signatures)
  are the diagnostics of `session.analyze(file)`. `session.compile`
  returns them as its `Err` for a program that has them.
- **Runtime errors** (overflow, out-of-bounds, an `Err` or `None` that
  bubbled to the top) return as `Err(VmError)` from `vm.run_program`.
- **`panic(...)` in silt code** reaches Rust as an `Err(VmError)` whose
  message carries the panicked string.
- **An `Err` from a host function** becomes a runtime error whose message
  starts with the function's name: `mylib.parse: bad input`. So does a
  result that is not of the type its signature returns.
- **A panic inside a host function** is caught and becomes a runtime
  error (`host function 'mylib.parse' panicked: ...`). The scheduler
  worker survives and other tasks keep running. Returning `Err` is still
  the way to fail; the catch is a safety net.

```rust
match vm.run_program(&program) {
    Ok(value) => println!("result: {:?}", value),
    Err(e) => eprintln!("silt error: {}", e.message),
}
```

Silt code has no access to the host filesystem, network or environment
beyond what the stdlib and your host modules provide.
