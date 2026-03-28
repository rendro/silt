# Concurrency Guide

Silt provides built-in concurrency based on the CSP (Communicating Sequential
Processes) model. This guide covers channels, tasks, select, and the cooperative
scheduler that powers it all.

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

```
-- Unbuffered channel (rendezvous)
let ch = chan()

-- Buffered channel with capacity 10
let ch = chan(10)
```

`chan()` creates a channel whose type is inferred from how it is used. You can
send any value type through a channel -- integers, strings, lists, records, even
other channels.

### Sending values

```
send(ch, "hello")
send(ch, 42)
send(ch, [1, 2, 3])
```

`send(ch, value)` places a value into the channel. If the channel's buffer is
full (or if it is unbuffered and no receiver is waiting), the sender blocks
until space becomes available.

### Receiving values

```
let msg = receive(ch)
```

`receive(ch)` takes the next value from the channel. If the channel is empty,
the receiver blocks until a value arrives.

### Unbuffered channels (rendezvous)

An unbuffered channel created with `chan()` has no internal buffer. This creates
a *rendezvous point*: the sender blocks until a receiver is ready, and the
receiver blocks until a sender is ready. The value is handed directly from one
task to the other.

```
-- Task A                      -- Task B
send(ch, "ping")      <--->    let msg = receive(ch)
-- A blocks here until         -- B blocks here until
-- B calls receive             -- A calls send
```

Use unbuffered channels when you want tight synchronization between tasks.

### Buffered channels

A buffered channel created with `chan(n)` can hold up to `n` values. Sends
succeed immediately as long as the buffer is not full. Receives succeed
immediately as long as the buffer is not empty.

```
let ch = chan(3)

send(ch, 1)   -- succeeds immediately (buffer: [1])
send(ch, 2)   -- succeeds immediately (buffer: [1, 2])
send(ch, 3)   -- succeeds immediately (buffer: [1, 2, 3])
-- send(ch, 4) would block here -- buffer is full

let a = receive(ch)  -- a = 1, buffer: [2, 3]
let b = receive(ch)  -- b = 2, buffer: [3]
```

Use buffered channels when the producer and consumer run at different speeds and
you want to decouple them.

### Channels carry any value type

Channels are not restricted to a single primitive type. You can send anything:

```
let ch = chan(5)
send(ch, [1, 2, 3])           -- a list
send(ch, (42, "hello"))       -- a tuple
send(ch, User { name: "Alice", age: 30, active: true })  -- a record
```

-----

## 3. Spawning Tasks

### The spawn keyword

`spawn` takes a zero-argument function and runs it as a concurrent task,
returning a `Handle` that you can use to join or cancel it later.

```
let handle = spawn fn() {
  -- this runs concurrently
  let result = expensive_computation()
  send(ch, result)
}
```

The function passed to `spawn` is a closure -- it captures variables from the
surrounding scope. Since all values in Silt are immutable, this is always safe.

```
let multiplier = 10
let ch = chan(10)

let h = spawn fn() {
  -- captures `multiplier` and `ch` from outer scope
  send(ch, multiplier * 2)
}

join(h)
receive(ch)  -- 20
```

### Tasks run cooperatively

Silt tasks are **not** OS threads. They run on a single thread and yield control
at channel operations (`send`, `receive`, `select`) and at `join`. Between those
yield points, a task runs without interruption.

This means:

- Tasks do not run in parallel -- only one task executes at a time.
- A task that does a long computation without touching a channel will not yield.
- The scheduler advances tasks in round-robin order when they block on channels.

### Example: producer/consumer

```
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, "hello")
    send(ch, "world")
  }

  let consumer = spawn fn() {
    let msg1 = receive(ch)
    let msg2 = receive(ch)
    println("{msg1} {msg2}")
  }

  join(producer)
  join(consumer)
}
```

What happens step by step:

1. `main` creates a buffered channel and spawns two tasks.
2. `join(producer)` tells the scheduler to run pending tasks until `producer`
   completes.
3. The producer sends `"hello"` and `"world"` into the channel.
4. `join(consumer)` runs the consumer, which receives both messages and prints
   them.

-----

## 4. Joining and Cancellation

### join(handle)

`join` blocks the current task until the spawned task completes, then returns
its result -- the value of the last expression in the spawned function's body.

```
let h = spawn fn() {
  42
}

let result = join(h)  -- result = 42
```

If the spawned task fails (runtime error), `join` propagates the error.

### cancel(handle)

`cancel` requests that a task be stopped. The task is marked as cancelled and
will not run further.

```
let h = spawn fn() {
  -- some work
  42
}
cancel(h)
-- h will not complete; joining it would return an error
```

### Pattern: spawn, work, join

The most common pattern is to spawn several tasks, let them do their work, and
then join them to collect results.

```
fn main() {
  let ch = chan(10)

  let h1 = spawn fn() {
    send(ch, 1)
  }

  let h2 = spawn fn() {
    send(ch, 2)
  }

  let h3 = spawn fn() {
    send(ch, 3)
  }

  join(h1)
  join(h2)
  join(h3)

  let a = receive(ch)
  let b = receive(ch)
  let c = receive(ch)
  println("sum = {a + b + c}")  -- sum = 6
}
```

-----

## 5. Channel Closing and Non-Blocking Operations

### Closing a channel

When a producer is done sending values, it can close the channel with `close(ch)`.
After closing:

- **Sends** on the closed channel will error.
- **Receives** return any remaining buffered values. Once the buffer is empty,
  `receive` returns `None` instead of blocking.

This lets the consumer detect when the producer is finished without needing to
know how many values to expect.

```
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, 1)
    send(ch, 2)
    send(ch, 3)
    close(ch)
  }

  let consumer = spawn fn() {
    let a = receive(ch)   -- 1
    let b = receive(ch)   -- 2
    let c = receive(ch)   -- 3
    let d = receive(ch)   -- None (closed and empty)
    println("{a} {b} {c} done={d}")
  }

  join(producer)
  join(consumer)
}
```

### Non-blocking send: try_send

`try_send(ch, value)` attempts to send without blocking. It returns `true` if
the value was placed in the channel, or `false` if the buffer is full or the
channel is closed.

```
let ch = chan(2)
try_send(ch, "a")   -- true
try_send(ch, "b")   -- true
try_send(ch, "c")   -- false (buffer full)
```

This is useful when you want to offer a value to a channel without stalling the
current task -- for example, logging to a channel that a consumer may or may not
be draining.

### Non-blocking receive: try_receive

`try_receive(ch)` attempts to receive without blocking. It returns `Some(value)`
if data is available, or `None` if the channel is empty or closed.

```
let ch = chan(10)
send(ch, 42)

try_receive(ch)   -- Some(42)
try_receive(ch)   -- None (empty)
```

This is useful for polling a channel in a loop or checking for data availability
without committing to a blocking wait.

### Combining close with try_receive

A common pattern is for a consumer to drain a channel until it is closed:

```
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, "a")
    send(ch, "b")
    send(ch, "c")
    close(ch)
  }

  join(producer)

  -- Drain remaining values
  let v1 = try_receive(ch)   -- Some("a")
  let v2 = try_receive(ch)   -- Some("b")
  let v3 = try_receive(ch)   -- Some("c")
  let v4 = try_receive(ch)   -- None (closed and empty)
  println("{v1} {v2} {v3} {v4}")
}
```

-----

## 6. Select

`select` lets a task wait on multiple channels at once and proceed with
whichever channel has data available first.

### Syntax

```
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

```
fn main() {
  let ch1 = chan(10)
  let ch2 = chan(10)

  let p1 = spawn fn() {
    send(ch1, "from producer 1")
  }

  let p2 = spawn fn() {
    send(ch2, "from producer 2")
  }

  -- Wait until one of them produces a value
  join(p1)
  join(p2)

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

```
fn fan_in(sources, output) {
  -- Read one value from whichever source is ready first
  let msg = select {
    receive(sources.ch1) as msg -> msg
    receive(sources.ch2) as msg -> msg
  }
  send(output, msg)
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

- `send(ch, val)` -- yields if the channel buffer is full
- `receive(ch)` -- yields if the channel is empty
- `select { ... }` -- yields if no channel has data
- `join(handle)` -- yields while the target task has not completed

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
- **Cancelled** -- the task was cancelled via `cancel(handle)`.

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

- A `send` finds the buffer full, but no tasks can run to drain it.
- A `receive` finds the buffer empty, but no tasks can run to fill it.
- A `join` is waiting for a task that is not making progress.
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

```
fn main() {
  let ch = chan(10)

  let producer = spawn fn() {
    send(ch, 10)
    send(ch, 20)
    send(ch, 30)
  }

  let consumer = spawn fn() {
    let a = receive(ch)
    let b = receive(ch)
    let c = receive(ch)
    println("sum = {a + b + c}")
  }

  join(producer)
  join(consumer)
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

```
fn main() {
  let jobs = chan(10)
  let results = chan(10)

  -- Enqueue work
  send(jobs, 10)
  send(jobs, 20)
  send(jobs, 30)

  -- Spawn workers that read from jobs and write to results
  let w1 = spawn fn() {
    let n = receive(jobs)
    send(results, n * 2)
  }

  let w2 = spawn fn() {
    let n = receive(jobs)
    send(results, n * 2)
  }

  let w3 = spawn fn() {
    let n = receive(jobs)
    send(results, n * 2)
  }

  join(w1)
  join(w2)
  join(w3)

  -- Collect results
  let a = receive(results)
  let b = receive(results)
  let c = receive(results)
  println("results: {a}, {b}, {c}")
  -- output: results: 20, 40, 60
}
```

Each worker picks up one job from the `jobs` channel, processes it, and sends
the result to the `results` channel. The work is distributed automatically --
whichever worker calls `receive(jobs)` first gets the next job.

### Pipeline (chain of channels)

Connect tasks in a linear chain where each stage reads from one channel and
writes to the next.

```
    [Stage 1] ----> [ch1] ----> [Stage 2] ----> [ch2] ----> [Stage 3]
```

```
fn main() {
  let raw = chan(10)
  let doubled = chan(10)
  let results = chan(10)

  -- Stage 1: produce raw values
  let s1 = spawn fn() {
    send(raw, 1)
    send(raw, 2)
    send(raw, 3)
  }

  -- Stage 2: double each value
  let s2 = spawn fn() {
    let a = receive(raw)
    let b = receive(raw)
    let c = receive(raw)
    send(doubled, a * 2)
    send(doubled, b * 2)
    send(doubled, c * 2)
  }

  -- Stage 3: sum the doubled values
  let s3 = spawn fn() {
    let a = receive(doubled)
    let b = receive(doubled)
    let c = receive(doubled)
    send(results, a + b + c)
  }

  join(s1)
  join(s2)
  join(s3)

  let total = receive(results)
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
  up after 5 seconds." A `receive` on an empty channel with no producer will
  deadlock (though Silt detects and reports this).

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
| Create channel | `chan()` / `chan(n)` | Communication between tasks |
| Send | `send(ch, val)` | Put a value into a channel |
| Receive | `receive(ch)` | Take a value from a channel |
| Spawn | `spawn fn() { ... }` | Run a function as a concurrent task |
| Join | `join(handle)` | Wait for a task to finish, get its result |
| Cancel | `cancel(handle)` | Stop a task |
| Close | `close(ch)` | Close a channel (no more sends) |
| Try send | `try_send(ch, val)` | Non-blocking send (returns Bool) |
| Try receive | `try_receive(ch)` | Non-blocking receive (returns Option) |
| Select | `select { receive(ch) as x -> ... }` | Wait on multiple channels |

The mental model: tasks are independent workers. Channels are the pipes between
them. `select` is a multiplexer. `join` is a synchronization barrier. Everything
else -- the scheduler, the task states, the deadlock detection -- is machinery
that makes this model work reliably under the hood.
