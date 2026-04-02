# Concurrency Guide

Silt provides built-in concurrency based on the CSP (Communicating Sequential
Processes) model with a **cooperative, single-threaded scheduler**. Tasks
interleave on one OS thread -- they do not run in parallel. This guide covers
channels, tasks, `channel.select`, and the cooperative scheduler that powers
it all.

All concurrency primitives are module-qualified: channels live in the `channel`
module, tasks live in the `task` module. There are no concurrency keywords --
`chan`, `send`, `receive`, and `spawn` were removed as keywords and replaced
with module functions.

-----

## 1. The CSP Concurrency Model

### What is CSP?

CSP -- Communicating Sequential Processes -- is a concurrency model where
independent tasks communicate by passing messages through channels rather than
by sharing memory. The idea comes from Tony Hoare's 1978 paper and is the same
model used by Go's goroutines and channels.

The core principle:

> **Do not communicate by sharing memory; share memory by communicating.**

In CSP, each task runs its own sequential code. When two tasks need to
coordinate, one sends a value into a channel and the other receives it. There is
no shared mutable state, no locks, and no data races.

**Note:** Silt v1 implements CSP with cooperative scheduling on a single thread.
Tasks are coroutines that yield at channel operations, not OS threads or
goroutines that run in parallel. The CSP *API* (channels, spawn, select) is the
same as in Go or Erlang, but the *execution model* is cooperative interleaving,
not preemptive parallelism. See Section 7 for details.

### Why CSP for Silt?

Silt is fully immutable -- every binding is `let`, there is no mutation, and
data structures are never modified in place. This makes CSP a natural fit:

- **Immutability eliminates data races.** Since values cannot be mutated, it is
  always safe to send any value through a channel. The receiver gets its own
  copy. There is nothing to corrupt.
- **Channels are the only coordination mechanism.** No mutexes, no atomics, no
  `synchronized` blocks. If two tasks need to interact, they use a channel.
- **Code stays sequential.** Each task reads like straight-line code. The
  concurrency is in how tasks are wired together, not in how individual tasks
  are written.

### How CSP compares to other models

| Model | Key idea | Tradeoff |
|---|---|---|
| **Threads + locks** | Shared memory protected by mutexes | Deadlocks, data races, hard to reason about |
| **Async/await** | Cooperative futures on an event loop | Colored functions, viral `async`, complex lifetimes |
| **Actors** | Each actor has private state, communicates via mailboxes | Untyped messages, hard to do request/response |
| **CSP (Silt)** | Independent tasks, typed channels, `channel.select` | No shared state (by design), cooperative single-threaded scheduling in v1 |

CSP sits between actors and raw threads. Like actors, tasks do not share state.
Unlike actors, channels are first-class values that can be passed around, and
`channel.select` lets a task wait on multiple channels at once.

Unlike Go's goroutines, which are multiplexed onto OS threads by a preemptive
runtime, Silt tasks are coroutines on a single thread. They yield only at
channel operations and `task.join`. The CSP API is forward-compatible with a
future preemptive runtime -- user code would not need to change.

-----

## 2. Channels

A channel is a typed conduit for passing values between tasks. Silt channels
come in two flavors: unbuffered and buffered.

### Creating channels

```silt
-- Unbuffered channel (rendezvous)
let ch = channel.new()

-- Buffered channel with capacity 10
let ch = channel.new(10)
```

`channel.new()` creates a channel whose type is inferred from how it is used.
You can send any value type through a channel -- integers, strings, lists,
records, even other channels.

### Sending values

```silt
channel.send(ch, "hello")
channel.send(ch, 42)
channel.send(ch, [1, 2, 3])
```

`channel.send(ch, value)` places a value into the channel. If the channel's
buffer is full, the sending task yields until space becomes available. (See the
note on unbuffered channels below for how this works with `channel.new()`.)

### Receiving values

```silt
let Message(msg) = channel.receive(ch)
```

`channel.receive(ch)` returns `Message(value)` when a value is available, or
`Closed` when the channel is closed and drained. If the channel is empty, the
receiver blocks until a value arrives.

### Unbuffered channels (rendezvous)

`channel.new()` creates a channel with no explicit buffer. Conceptually, this is
a *rendezvous point*: the sender and receiver synchronize around a single value.

```
-- Task A                                    -- Task B
channel.send(ch, "ping")      <--->    let Message(msg) = channel.receive(ch)
-- A blocks here until                       -- B calls channel.receive to
-- B calls channel.receive                   -- pick up the value
```

**Implementation note (cooperative model):** In a preemptive runtime, an
unbuffered channel would park the sender mid-execution until a receiver appears.
Because Silt v1 uses cooperative scheduling, a task cannot be suspended inside
`channel.send` -- it must either complete the send or yield. In practice,
"unbuffered" channels are promoted to capacity-1 internally: the send succeeds
immediately (placing the value in a single-slot buffer), and the sender blocks
only if that slot is already occupied. This means a `channel.new()` send can
complete before any receiver is ready, which differs from true rendezvous
semantics in Go. For most patterns this is transparent, but it matters if you
rely on send-blocking-until-received for synchronization.

Use unbuffered channels when you want tight synchronization between tasks. Be
aware that in v1, the synchronization is "at most one value in flight" rather
than strict rendezvous.

### Buffered channels

A buffered channel created with `channel.new(n)` can hold up to `n` values.
Sends succeed immediately as long as the buffer is not full. Receives succeed
immediately as long as the buffer is not empty.

```silt
let ch = channel.new(3)

channel.send(ch, 1)   -- succeeds immediately (buffer: [1])
channel.send(ch, 2)   -- succeeds immediately (buffer: [1, 2])
channel.send(ch, 3)   -- succeeds immediately (buffer: [1, 2, 3])
-- channel.send(ch, 4) would block here -- buffer is full

let Message(a) = channel.receive(ch)  -- a = 1, buffer: [2, 3]
let Message(b) = channel.receive(ch)  -- b = 2, buffer: [3]
```

Use buffered channels when the producer and consumer run at different speeds and
you want to decouple them.

### Channels carry any value type

Channels are not restricted to a single primitive type. You can send anything:

```silt
let ch = channel.new(5)
channel.send(ch, [1, 2, 3])           -- a list
channel.send(ch, (42, "hello"))       -- a tuple
channel.send(ch, User { name: "Alice", age: 30, active: true })  -- a record
```

-----

## 3. Spawning Tasks

### `task.spawn`

`task.spawn` takes a zero-argument function and runs it as a concurrent task,
returning a `Handle` that you can use to join or cancel it later.

```silt
let handle = task.spawn(fn() {
  -- this runs concurrently
  let result = expensive_computation()
  channel.send(ch, result)
})
```

The function passed to `task.spawn` is a closure -- it captures variables from
the surrounding scope. Since all values in Silt are immutable, this is always
safe.

```silt
let multiplier = 10
let ch = channel.new(10)

let h = task.spawn(fn() {
  -- captures `multiplier` and `ch` from outer scope
  channel.send(ch, multiplier * 2)
})

task.join(h)
let Message(val) = channel.receive(ch)  -- val = 20
```

### Tasks run cooperatively (not in parallel)

Silt tasks are **coroutines**, not OS threads or green threads. They run on a
single OS thread and yield control only at explicit yield points:

- `channel.send(ch, val)` -- yields if the buffer is full
- `channel.receive(ch)` -- yields if the buffer is empty
- `channel.select([...])` -- yields if no channel has data
- `task.join(handle)` -- yields while the target task is incomplete

Between those yield points, a task runs without interruption. There is no
preemption and no time-slicing.

This means:

- **No parallelism.** Only one task executes at a time. Multiple CPU cores are
  not utilized.
- **CPU-bound tasks block everything.** A task that does a long computation
  without touching a channel will not yield, starving all other tasks.
- **Deterministic scheduling.** The same inputs always produce the same task
  interleaving. This makes concurrent programs reproducible and easy to test.
- The scheduler advances tasks in round-robin order when they block on channels.

### Example: producer/consumer

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "world")
  })

  let consumer = task.spawn(fn() {
    let Message(msg1) = channel.receive(ch)
    let Message(msg2) = channel.receive(ch)
    println("{msg1} {msg2}")
  })

  task.join(producer)
  task.join(consumer)
}
```

What happens step by step:

1. `main` creates a buffered channel and spawns two tasks.
2. `task.join(producer)` tells the scheduler to run pending tasks until
   `producer` completes.
3. The producer sends `"hello"` and `"world"` into the channel.
4. `task.join(consumer)` runs the consumer, which receives both messages (as
   `Message(value)`) and prints them.

-----

## 4. Joining and Cancellation

### `task.join(handle)`

`task.join` blocks the current task until the spawned task completes, then
returns its result -- the value of the last expression in the spawned function's
body.

```silt
let h = task.spawn(fn() {
  42
})

let result = task.join(h)  -- result = 42
```

If the spawned task fails (runtime error), `task.join` propagates the error.

### `task.cancel(handle)`

`task.cancel` requests that a task be stopped. The task is marked as cancelled
and will not run further.

```silt
let h = task.spawn(fn() {
  -- some work
  42
})
task.cancel(h)
-- h will not complete; joining it would return an error
```

### Pattern: spawn, work, join

The most common pattern is to spawn several tasks, let them do their work, and
then join them to collect results.

```silt
fn main() {
  let ch = channel.new(10)

  let h1 = task.spawn(fn() {
    channel.send(ch, 1)
  })

  let h2 = task.spawn(fn() {
    channel.send(ch, 2)
  })

  let h3 = task.spawn(fn() {
    channel.send(ch, 3)
  })

  task.join(h1)
  task.join(h2)
  task.join(h3)

  let Message(a) = channel.receive(ch)
  let Message(b) = channel.receive(ch)
  let Message(c) = channel.receive(ch)
  println("sum = {a + b + c}")  -- sum = 6
}
```

-----

## 5. Channel Closing and Non-Blocking Operations

### Closing a channel

When a producer is done sending values, it can close the channel with
`channel.close(ch)`. After closing:

- **Sends** on the closed channel will error.
- **Receives** return any remaining buffered values as `Message(value)`. Once
  the buffer is empty, `channel.receive` returns `Closed` instead of blocking.

This lets the consumer detect when the producer is finished without needing to
know how many values to expect.

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.send(ch, 3)
    channel.close(ch)
  })

  let consumer = task.spawn(fn() {
    let Message(a) = channel.receive(ch)   -- 1
    let Message(b) = channel.receive(ch)   -- 2
    let Message(c) = channel.receive(ch)   -- 3
    let d = channel.receive(ch)            -- Closed (closed and empty)
    println("{a} {b} {c} done={d}")
  })

  task.join(producer)
  task.join(consumer)
}
```

### Non-blocking send: `channel.try_send`

`channel.try_send(ch, value)` attempts to send without blocking. It returns
`true` if the value was placed in the channel, or `false` if the buffer is full
or the channel is closed.

```silt
let ch = channel.new(2)
channel.try_send(ch, "a")   -- true
channel.try_send(ch, "b")   -- true
channel.try_send(ch, "c")   -- false (buffer full)
```

This is useful when you want to offer a value to a channel without stalling the
current task -- for example, logging to a channel that a consumer may or may not
be draining.

### Non-blocking receive: `channel.try_receive`

`channel.try_receive(ch)` attempts to receive without blocking. It returns
`Message(value)` if data is available, `Empty` if the channel has no data, or
`Closed` if the channel is closed and drained.

```silt
let ch = channel.new(10)
channel.send(ch, 42)

channel.try_receive(ch)   -- Message(42)
channel.try_receive(ch)   -- Empty
```

This is useful for polling a channel in a loop or checking for data availability
without committing to a blocking wait. Unlike the old `Option`-based API, you can
distinguish between "no data yet" (`Empty`) and "no data ever" (`Closed`).

### Combining close with `channel.try_receive`

A common pattern is for a consumer to drain a channel until it is closed:

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "a")
    channel.send(ch, "b")
    channel.send(ch, "c")
    channel.close(ch)
  })

  task.join(producer)

  -- Drain remaining values
  let Message(v1) = channel.try_receive(ch)   -- "a"
  let Message(v2) = channel.try_receive(ch)   -- "b"
  let Message(v3) = channel.try_receive(ch)   -- "c"
  let v4 = channel.try_receive(ch)            -- Closed
  println("{v1} {v2} {v3} {v4}")
}
```

-----

## 6. Select and Each

`channel.select` lets a task wait on multiple channels at once and proceed with
whichever channel has data available first. It takes a list of channels and
returns a `(channel, Message(value))` tuple identifying which channel produced
the value, or `(channel, Closed)` when all channels are closed and drained.

Use the `^` pin operator to match against channel identities in the result:

### Syntax

```silt
match channel.select([ch1, ch2]) {
  (^ch1, Message(msg)) -> handle_first(msg)
  (^ch2, Message(msg)) -> handle_second(msg)
  (_, Closed) -> println("all channels closed")
  _ -> panic("unexpected")
}
```

The `^` pin operator matches against the current value of an existing variable
instead of creating a new binding. `^ch1` means "match if this is the same
channel as `ch1`", not "bind a new variable called ch1". The pin operator works
in any pattern position, not just with `channel.select`.

`channel.select` polls the given channels in order and returns the first one
that has data. If no channel is ready, the scheduler runs pending tasks
and tries again. When all channels are closed and empty, it returns
`(channel, Closed)` instead of deadlocking.

### Example: multiplexing two producers

```silt
fn main() {
  let ch1 = channel.new(10)
  let ch2 = channel.new(10)

  let p1 = task.spawn(fn() {
    channel.send(ch1, "from producer 1")
  })

  let p2 = task.spawn(fn() {
    channel.send(ch2, "from producer 2")
  })

  -- Wait until one of them produces a value
  task.join(p1)
  task.join(p2)

  let result = match channel.select([ch1, ch2]) {
    (^ch1, Message(msg)) -> msg
    (^ch2, Message(msg)) -> msg
    (_, Closed) -> "all closed"
    _ -> panic("unexpected")
  }

  println(result)
}
```

### Pattern: fan-in

`channel.select` is the building block for fan-in -- combining multiple input
channels into a single stream.

```silt
fn fan_in(ch1, ch2, output) {
  -- Read one value from whichever source is ready first
  let msg = match channel.select([ch1, ch2]) {
    (_, Message(msg)) -> msg
    (_, Closed) -> return ()
  }
  channel.send(output, msg)
}
```

### `channel.each`

For the common case of draining a single channel until it closes, use
`channel.each`. It calls a function for each received message and returns
`Unit` when the channel closes:

```silt
fn main() {
  let ch = channel.new(10)
  channel.send(ch, "hello")
  channel.send(ch, "world")
  channel.close(ch)

  channel.each(ch) { msg ->
    println("got: {msg}")
  }
  println("done")
}
```

This is the channel equivalent of `list.each`.

### Deadlock detection

If all channels passed to `channel.select` are empty and no other task can make
progress (no ready tasks in the scheduler), Silt detects the deadlock and
reports an error:

```
channel.select: deadlock detected - no channels have data and no tasks can make progress
```

-----

## 7. The Cooperative Scheduler

Understanding the scheduler is essential for reasoning about how concurrent Silt
code actually executes. Silt v1 uses **cooperative scheduling on a single OS
thread**. This is a deliberate design choice, not a temporary limitation -- it
prioritizes determinism, simplicity, and safety over raw parallelism.

### Single-threaded, cooperative

Silt's scheduler runs on a single OS thread. Tasks yield at well-defined points:

- `channel.send(ch, val)` -- yields if the channel buffer is full
- `channel.receive(ch)` -- yields if the channel is empty
- `channel.select([...])` -- yields if no channel has data
- `task.join(handle)` -- yields while the target task has not completed

Between yield points, a task runs to completion of its current expression
without interruption. There is no preemption, no time-slicing, and no
parallelism.

### Task states

Each task in the scheduler is in one of five states:

```
Ready  ---------> Running ---------> Completed
  ^                  |
  |                  v
  +---- BlockedSend (channel buffer full)
  |
  +---- BlockedReceive (channel buffer empty)
  |
  +---- Cancelled
```

- **Ready** -- the task is eligible to run.
- **BlockedSend** -- the task tried to send but the channel buffer is full. It
  becomes Ready when space opens up.
- **BlockedReceive** -- the task tried to receive but the channel is empty. It
  becomes Ready when a value arrives.
- **Completed** -- the task finished executing. Its result is stored in its
  handle.
- **Cancelled** -- the task was cancelled via `task.cancel(handle)`.

### How scheduling works

When a blocking operation cannot proceed immediately, the scheduler:

1. Takes all Ready tasks from the queue.
2. Runs each one (evaluates its body expression).
3. If a task completes, stores its result and marks it Completed.
4. Returns any still-blocked tasks to the queue.
5. Tries the original operation again.

This loop repeats until the operation succeeds or the scheduler detects a
deadlock (no tasks made progress).

**Fan-out note:** Because scheduling is cooperative and there is no
round-robin between yield points, a worker that loops on `channel.receive`
may consume all messages before other workers get a turn. Fan-out patterns
are structurally concurrent but execute serially. This will be addressed
when the interpreter moves to a bytecode VM with instruction-level
preemption.

### Deadlock detection

The scheduler detects deadlocks when:

- A `channel.send` finds the buffer full, but no tasks can run to drain it.
- A `channel.receive` finds the buffer empty, but no tasks can run to fill it.
- A `task.join` is waiting for a task that is not making progress.
- A `channel.select` finds all channels empty and no tasks can produce data.

In each case, Silt reports a clear error message rather than hanging forever.

### Why cooperative over preemptive?

| Cooperative | Preemptive |
|---|---|
| Deterministic execution order | Non-deterministic interleaving |
| No thread-safety overhead | Requires locks, atomics, or ownership tracking |
| Simpler to implement and debug | Complex runtime, OS thread management |
| No parallelism (single-threaded) | True parallelism across CPU cores |
| Tasks must yield voluntarily | Tasks can be interrupted at any point |

Silt chose cooperative scheduling because it aligns with the language's
philosophy: simplicity, safety, and predictability. Combined with full
immutability, cooperative scheduling means concurrent programs are deterministic
-- the same inputs always produce the same outputs, in the same order.

The downside is no true parallelism. CPU-bound workloads get no speedup from
spawning tasks. This is acceptable for v1 -- the CSP API (`channel.new`,
`channel.send`, `channel.receive`, `task.spawn`, `channel.select`) is
forward-compatible with a preemptive, multi-threaded runtime in a future
version. User code would not change; only the scheduler implementation would.

-----

## 8. Common Patterns

### Producer/Consumer

The most fundamental pattern. One task produces data, another consumes it, and a
channel connects them.

```
    Producer ----> [Channel] ----> Consumer
```

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, 10)
    channel.send(ch, 20)
    channel.send(ch, 30)
  })

  let consumer = task.spawn(fn() {
    let Message(a) = channel.receive(ch)
    let Message(b) = channel.receive(ch)
    let Message(c) = channel.receive(ch)
    println("sum = {a + b + c}")
  })

  task.join(producer)
  task.join(consumer)
}
```

### Fan-out / Fan-in (multiple workers)

Distribute work across multiple tasks, then collect results into a single
channel.

```
              +---> [Worker 1] ---+
              |                   |
    [jobs] ---+---> [Worker 2] ---+---> [results]
              |                   |
              +---> [Worker 3] ---+
```

```silt
fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)

  -- Enqueue work
  channel.send(jobs, 10)
  channel.send(jobs, 20)
  channel.send(jobs, 30)

  -- Spawn workers that read from jobs and write to results
  let w1 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w2 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w3 = task.spawn(fn() {
    let Message(n) = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  task.join(w1)
  task.join(w2)
  task.join(w3)

  -- Collect results
  let Message(a) = channel.receive(results)
  let Message(b) = channel.receive(results)
  let Message(c) = channel.receive(results)
  println("results: {a}, {b}, {c}")
  -- output: results: 20, 40, 60
}
```

Each worker picks up one job from the `jobs` channel, processes it, and sends
the result to the `results` channel. The work is distributed automatically --
whichever worker calls `channel.receive(jobs)` first gets the next job.

### Pipeline (chain of channels)

Connect tasks in a linear chain where each stage reads from one channel and
writes to the next.

```
    [Stage 1] ----> [ch1] ----> [Stage 2] ----> [ch2] ----> [Stage 3]
```

```silt
fn main() {
  let raw = channel.new(10)
  let doubled = channel.new(10)
  let results = channel.new(10)

  -- Stage 1: produce raw values
  let s1 = task.spawn(fn() {
    channel.send(raw, 1)
    channel.send(raw, 2)
    channel.send(raw, 3)
  })

  -- Stage 2: double each value
  let s2 = task.spawn(fn() {
    let Message(a) = channel.receive(raw)
    let Message(b) = channel.receive(raw)
    let Message(c) = channel.receive(raw)
    channel.send(doubled, a * 2)
    channel.send(doubled, b * 2)
    channel.send(doubled, c * 2)
  })

  -- Stage 3: sum the doubled values
  let s3 = task.spawn(fn() {
    let Message(a) = channel.receive(doubled)
    let Message(b) = channel.receive(doubled)
    let Message(c) = channel.receive(doubled)
    channel.send(results, a + b + c)
  })

  task.join(s1)
  task.join(s2)
  task.join(s3)

  let Message(total) = channel.receive(results)
  println("pipeline total = {total}")
  -- output: pipeline total = 12
}
```

Each stage is independent and only knows about its input and output channels.
This makes pipelines easy to extend -- just add another stage in the middle.

-----

## 9. Limitations and Future Work

### Current limitations (v1)

- **Single-threaded, cooperative scheduling.** Tasks are coroutines on a single
  OS thread. They interleave but never execute simultaneously. CPU-bound work
  gets no speedup from spawning tasks. A task that performs heavy computation
  between yield points blocks all other tasks until it yields.

- **Tasks yield only at channel operations and `task.join`.** There is no
  preemption. If a task enters a long-running `loop` that never touches a
  channel, no other task can make progress until that loop completes.

- **Unbuffered channels are capacity-1.** Because the cooperative scheduler
  cannot park a sender mid-execution, `channel.new()` is internally promoted to
  a single-slot buffer. A send on an "unbuffered" channel succeeds immediately
  if the slot is empty, which differs from true rendezvous semantics where the
  sender blocks until a receiver is ready.

- **Deterministic scheduling.** This is both a strength and a limitation. The
  same inputs always produce the same interleaving, which is excellent for
  testing and debugging. However, it means performance characteristics are
  tightly coupled to task ordering -- you cannot rely on "whichever task
  finishes first" for load balancing the way you would with OS threads.

- **No timeouts.** There is no way to say "receive from this channel, but give
  up after 5 seconds." A `channel.receive` on an empty channel with no producer
  will deadlock (though Silt detects and reports this).

- **No buffering changes after creation.** A channel's capacity is fixed at
  creation time. You cannot resize a buffer or convert between buffered and
  unbuffered.

- **Select only supports receive.** `channel.select` polls channels for
  incoming data. You cannot select on send operations.

### Forward compatibility

The CSP API (`channel.new`, `channel.send`, `channel.receive`, `task.spawn`,
`channel.select`) is designed to be **runtime-agnostic**. User code written
against this API today will work without modification on a future preemptive
runtime. Only the scheduler implementation would change.

### Future work

- **Preemptive runtime.** Running tasks on a real async runtime (e.g., Tokio) or
  OS threads would enable true parallelism. Silt's full immutability makes this
  safe by default -- no data races, no locking needed.

- **True unbuffered channels.** A preemptive scheduler would allow genuine
  rendezvous semantics where the sender parks until a receiver is ready.

- **Timeouts and deadlines.** Adding a timeout parameter to `channel.select`
  would enable patterns like "wait for a response, but give up after 100ms."

- **Buffered send in select.** Extending `channel.select` to support send
  operations in addition to receive.

-----

## Summary

| Concept | Syntax | Purpose |
|---|---|---|
| Create channel | `channel.new()` / `channel.new(n)` | Communication between tasks |
| Send | `channel.send(ch, val)` | Put a value into a channel |
| Receive | `channel.receive(ch)` | Take a value from a channel (`Message(val)` or `Closed`) |
| Spawn | `task.spawn(fn() { ... })` | Run a function as a concurrent task |
| Join | `task.join(handle)` | Wait for a task to finish, get its result |
| Cancel | `task.cancel(handle)` | Stop a task |
| Close | `channel.close(ch)` | Close a channel (no more sends) |
| Try send | `channel.try_send(ch, val)` | Non-blocking send (returns Bool) |
| Try receive | `channel.try_receive(ch)` | Non-blocking receive (`Message(val)`, `Empty`, or `Closed`) |
| Select | `channel.select([ch1, ch2])` | Wait on multiple channels (returns `(ch, value)` tuple) |

The mental model: tasks are independent workers. Channels are the pipes between
them. `channel.select` is a multiplexer. `task.join` is a synchronization barrier.
Everything else -- the scheduler, the task states, the deadlock detection -- is
machinery that makes this model work reliably under the hood.

**Remember:** In v1, all of this runs cooperatively on a single thread. Tasks
interleave at channel operations but never execute in parallel. The API is the
same CSP model used by Go and Erlang, and is forward-compatible with a
preemptive runtime in a future version.
