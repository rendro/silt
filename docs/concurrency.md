# Concurrency Guide

Silt provides built-in concurrency based on the CSP (Communicating Sequential
Processes) model. This guide covers channels, tasks, select, and the cooperative
scheduler that powers it all.

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
| **CSP (Silt)** | Independent tasks, typed channels, select | No shared state (by design), single-threaded in v1 |

CSP sits between actors and raw threads. Like actors, tasks do not share state.
Unlike actors, channels are first-class values that can be passed around, and
`select` lets a task wait on multiple channels at once.

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
buffer is full (or if it is unbuffered and no receiver is waiting), the sender
blocks until space becomes available.

### Receiving values

```silt
let msg = channel.receive(ch)
```

`channel.receive(ch)` takes the next value from the channel. If the channel is
empty, the receiver blocks until a value arrives.

### Unbuffered channels (rendezvous)

An unbuffered channel created with `channel.new()` has no internal buffer. This
creates a *rendezvous point*: the sender blocks until a receiver is ready, and
the receiver blocks until a sender is ready. The value is handed directly from
one task to the other.

```
-- Task A                              -- Task B
channel.send(ch, "ping")      <--->    let msg = channel.receive(ch)
-- A blocks here until                 -- B blocks here until
-- B calls channel.receive             -- A calls channel.send
```

Use unbuffered channels when you want tight synchronization between tasks.

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

let a = channel.receive(ch)  -- a = 1, buffer: [2, 3]
let b = channel.receive(ch)  -- b = 2, buffer: [3]
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
channel.receive(ch)  -- 20
```

### Tasks run cooperatively

Silt tasks are **not** OS threads. They run on a single thread and yield control
at channel operations (`channel.send`, `channel.receive`, `select`) and at
`task.join`. Between those yield points, a task runs without interruption.

This means:

- Tasks do not run in parallel -- only one task executes at a time.
- A task that does a long computation without touching a channel will not yield.
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
    let msg1 = channel.receive(ch)
    let msg2 = channel.receive(ch)
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
4. `task.join(consumer)` runs the consumer, which receives both messages and
   prints them.

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

  let a = channel.receive(ch)
  let b = channel.receive(ch)
  let c = channel.receive(ch)
  println("sum = {a + b + c}")  -- sum = 6
}
```

-----

## 5. Channel Closing and Non-Blocking Operations

### Closing a channel

When a producer is done sending values, it can close the channel with
`channel.close(ch)`. After closing:

- **Sends** on the closed channel will error.
- **Receives** return any remaining buffered values. Once the buffer is empty,
  `channel.receive` returns `None` instead of blocking.

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
    let a = channel.receive(ch)   -- 1
    let b = channel.receive(ch)   -- 2
    let c = channel.receive(ch)   -- 3
    let d = channel.receive(ch)   -- None (closed and empty)
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
`Some(value)` if data is available, or `None` if the channel is empty or closed.

```silt
let ch = channel.new(10)
channel.send(ch, 42)

channel.try_receive(ch)   -- Some(42)
channel.try_receive(ch)   -- None (empty)
```

This is useful for polling a channel in a loop or checking for data availability
without committing to a blocking wait.

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
  let v1 = channel.try_receive(ch)   -- Some("a")
  let v2 = channel.try_receive(ch)   -- Some("b")
  let v3 = channel.try_receive(ch)   -- Some("c")
  let v4 = channel.try_receive(ch)   -- None (closed and empty)
  println("{v1} {v2} {v3} {v4}")
}
```

-----

## 6. Select

`select` lets a task wait on multiple channels at once and proceed with
whichever channel has data available first.

### Syntax

```silt
select {
  receive(ch1) as msg -> handle_first(msg)
  receive(ch2) as msg -> handle_second(msg)
}
```

Each arm specifies a channel to poll and a binding name for the received value.
The `select` expression evaluates the arms in order and takes the first one
whose channel has data. If no channel is ready, the scheduler runs pending tasks
and tries again.

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

  let result = select {
    receive(ch1) as msg -> msg
    receive(ch2) as msg -> msg
  }

  println(result)
}
```

### Pattern: fan-in

`select` is the building block for fan-in -- combining multiple input channels
into a single stream.

```silt
fn fan_in(sources, output) {
  -- Read one value from whichever source is ready first
  let msg = select {
    receive(sources.ch1) as msg -> msg
    receive(sources.ch2) as msg -> msg
  }
  channel.send(output, msg)
}
```

### Deadlock detection

If all arms of a `select` are waiting for data and no other task can make
progress (no ready tasks in the scheduler), Silt detects the deadlock and
reports an error:

```
select: deadlock detected - no channels have data and no tasks can make progress
```

-----

## 7. The Cooperative Scheduler

Understanding the scheduler helps you reason about how your concurrent code
actually executes.

### Single-threaded, cooperative

Silt's scheduler runs on a single OS thread. Tasks yield at well-defined points:

- `channel.send(ch, val)` -- yields if the channel buffer is full
- `channel.receive(ch)` -- yields if the channel is empty
- `select { ... }` -- yields if no channel has data
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

### Deadlock detection

The scheduler detects deadlocks when:

- A `channel.send` finds the buffer full, but no tasks can run to drain it.
- A `channel.receive` finds the buffer empty, but no tasks can run to fill it.
- A `task.join` is waiting for a task that is not making progress.
- A `select` finds all channels empty and no tasks can produce data.

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

The downside is no true parallelism, which is acceptable for v1 and can be
addressed in future versions (see Section 8).

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
    let a = channel.receive(ch)
    let b = channel.receive(ch)
    let c = channel.receive(ch)
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
    let n = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w2 = task.spawn(fn() {
    let n = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  let w3 = task.spawn(fn() {
    let n = channel.receive(jobs)
    channel.send(results, n * 2)
  })

  task.join(w1)
  task.join(w2)
  task.join(w3)

  -- Collect results
  let a = channel.receive(results)
  let b = channel.receive(results)
  let c = channel.receive(results)
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
    let a = channel.receive(raw)
    let b = channel.receive(raw)
    let c = channel.receive(raw)
    channel.send(doubled, a * 2)
    channel.send(doubled, b * 2)
    channel.send(doubled, c * 2)
  })

  -- Stage 3: sum the doubled values
  let s3 = task.spawn(fn() {
    let a = channel.receive(doubled)
    let b = channel.receive(doubled)
    let c = channel.receive(doubled)
    channel.send(results, a + b + c)
  })

  task.join(s1)
  task.join(s2)
  task.join(s3)

  let total = channel.receive(results)
  println("pipeline total = {total}")
  -- output: pipeline total = 12
}
```

Each stage is independent and only knows about its input and output channels.
This makes pipelines easy to extend -- just add another stage in the middle.

-----

## 9. Limitations and Future Work

### Current limitations (v1)

- **No true parallelism.** The cooperative scheduler runs on a single thread.
  Tasks interleave but never execute simultaneously. CPU-bound work gets no
  speedup from concurrency.

- **No timeouts.** There is no way to say "receive from this channel, but give
  up after 5 seconds." A `channel.receive` on an empty channel with no producer
  will deadlock (though Silt detects and reports this).

- **No buffering changes after creation.** A channel's capacity is fixed at
  creation time. You cannot resize a buffer or convert between buffered and
  unbuffered.

- **Select only supports receive.** The `select` expression polls channels for
  incoming data. You cannot select on send operations.

### Future work

- **Tokio integration.** Running tasks on a real async runtime would enable true
  parallelism and non-blocking I/O while keeping the same channel-based API.

- **Real OS threads.** For CPU-bound workloads, spawning tasks on separate
  threads would provide actual parallel execution. Silt's immutability makes
  this safe by default.

- **Timeouts and deadlines.** Adding a `timeout` arm to `select` would enable
  patterns like "wait for a response, but give up after 100ms."

- **Buffered send in select.** Extending `select` to support `send(ch, val) as
  _ -> ...` arms.

-----

## Summary

| Concept | Syntax | Purpose |
|---|---|---|
| Create channel | `channel.new()` / `channel.new(n)` | Communication between tasks |
| Send | `channel.send(ch, val)` | Put a value into a channel |
| Receive | `channel.receive(ch)` | Take a value from a channel |
| Spawn | `task.spawn(fn() { ... })` | Run a function as a concurrent task |
| Join | `task.join(handle)` | Wait for a task to finish, get its result |
| Cancel | `task.cancel(handle)` | Stop a task |
| Close | `channel.close(ch)` | Close a channel (no more sends) |
| Try send | `channel.try_send(ch, val)` | Non-blocking send (returns Bool) |
| Try receive | `channel.try_receive(ch)` | Non-blocking receive (returns Option) |
| Select | `select { receive(ch) as x -> ... }` | Wait on multiple channels |

The mental model: tasks are independent workers. Channels are the pipes between
them. `select` is a multiplexer. `task.join` is a synchronization barrier.
Everything else -- the scheduler, the task states, the deadlock detection -- is
machinery that makes this model work reliably under the hood.
