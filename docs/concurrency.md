---
title: "Concurrency"
order: 4
description: "silt's CSP concurrency model: parallel tasks, typed channels, select with pin patterns."
---

# Concurrency

Silt provides built-in concurrency based on CSP (Communicating Sequential
Processes). Tasks communicate through channels. There is no shared mutable
state -- every value in silt is immutable, so sending a value through a channel
is always safe. Tasks are lightweight and run in parallel on a fixed thread
pool.

All concurrency primitives live in two modules: `channel` and `task`. There are
no concurrency keywords.


## 1. The Model

### CSP in one paragraph

CSP -- Communicating Sequential Processes -- is a concurrency model from Tony
Hoare's 1978 paper. Independent tasks run their own sequential code and
coordinate by sending messages through channels. No task can see another task's
variables. The only way to share data is to put it in a channel and let the
other side take it out. Go's goroutines and channels are the most well-known
modern implementation of CSP.

The core principle:

> **Do not communicate by sharing memory; share memory by communicating.**

### Why CSP fits silt

Silt is fully immutable. Every binding is `let`, data structures are never
modified in place, and there is no mutable reference. This makes CSP a natural
fit:

- **Immutability eliminates data races.** Any value can be sent through a
  channel without copying concerns or locking. The receiver gets exactly what
  the sender put in. Nothing can corrupt it.
- **Channels are the only coordination mechanism.** No mutexes, no atomics, no
  `synchronized` blocks. If two tasks need to interact, they use a channel.
- **Code stays sequential.** Each task reads like straight-line code. The
  concurrency is in how tasks are wired together via channels, not in how
  individual tasks are written.
- **Tasks share the environment via `Rc`.** This is safe because everything is
  immutable -- multiple tasks can hold references to the same values without
  risk of data races.

### How CSP compares to other models

| Model | Key idea | Tradeoff |
|---|---|---|
| **Threads + locks** | Shared memory protected by mutexes | Deadlocks, data races, hard to reason about |
| **Async/await** | Cooperative futures on an event loop | Colored functions, viral `async`, complex lifetimes |
| **Actors** | Each actor has private state, communicates via mailboxes | Untyped messages, hard to do request/response |
| **CSP (silt)** | Independent tasks, typed channels, `channel.select` | No shared state (by design), real parallelism |

Like Go's goroutines, `task.spawn` creates a lightweight task that runs in
parallel. Tasks are multiplexed onto a fixed-size thread pool (one thread per
CPU core), so spawning is cheap and you can run thousands of concurrent tasks
efficiently. Tasks coordinate through channels. Since all silt values are
immutable, there are no data races -- channels are the sole communication
mechanism.


## 2. Channels

A channel is a conduit for passing values between tasks.

### Creating channels

```silt
-- Unbuffered channel (capacity 0, promoted to 1 internally -- see below)
let ch = channel.new()

-- Buffered channel with capacity 10
let ch = channel.new(10)
```

`channel.new()` takes zero or one argument. With no argument, it creates an
unbuffered channel. With an integer argument, it creates a buffered channel with
that capacity.

Channels carry any value type -- integers, strings, lists, tuples, records,
even other channels. The type is inferred from usage.

### Sending: `channel.send(ch, value)`

```silt
channel.send(ch, "hello")
channel.send(ch, 42)
channel.send(ch, [1, 2, 3])
```

`channel.send` places a value into the channel's buffer. If the buffer is full,
the current task is parked until space opens up. The OS thread is not blocked --
it runs other tasks in the meantime. If the channel is closed, sending is an
error.

Returns `Unit`.

### Receiving: `channel.receive(ch)`

```silt
let Message(msg) = channel.receive(ch)
```

`channel.receive` takes one value from the channel's buffer. It returns one of
two variants:

- `Message(value)` -- a value was available.
- `Closed` -- the channel is closed and the buffer is empty. No more values
  will ever arrive.

If the buffer is empty but the channel is still open, the current task is
parked until a value arrives. The OS thread is not blocked -- it runs other
tasks in the meantime.

Before attempting to receive, the scheduler yields to other tasks so that
competing receivers get a fair turn (round-robin fan-out).

### Closing: `channel.close(ch)`

```silt
channel.close(ch)
```

Signals that no more values will be sent on this channel. After closing:

- **Sends** on the closed channel produce a runtime error.
- **Receives** drain any remaining buffered values as `Message(value)`. Once
  the buffer is empty, `channel.receive` returns `Closed`.

Returns `Unit`.

### Non-blocking send: `channel.try_send(ch, value)`

```silt
let ch = channel.new(2)
channel.try_send(ch, "a")   -- true
channel.try_send(ch, "b")   -- true
channel.try_send(ch, "c")   -- false (buffer full)
```

Attempts to send without blocking. Returns `true` if the value was placed in
the buffer, or `false` if the buffer is full or the channel is closed. Never
yields to the scheduler.

### Non-blocking receive: `channel.try_receive(ch)`

```silt
let ch = channel.new(10)
channel.send(ch, 42)

channel.try_receive(ch)   -- Message(42)
channel.try_receive(ch)   -- Empty
```

Attempts to receive without blocking. Returns one of three variants:

- `Message(value)` -- a value was available.
- `Empty` -- no data yet, but the channel is still open.
- `Closed` -- no data and the channel is closed.

This lets you distinguish "nothing right now" from "nothing ever again."

### Iterating: `channel.each(ch) { val -> ... }`

```silt
let ch = channel.new(10)
channel.send(ch, "hello")
channel.send(ch, "world")
channel.close(ch)

channel.each(ch) { msg ->
  println("got: {msg}")
}
-- prints:
--   got: hello
--   got: world
```

`channel.each` calls a function for each value received from the channel and
returns `Unit` when the channel closes. It is the channel equivalent of
`list.each`. When the channel is empty, the task is parked until new data
arrives.

After processing each message, `channel.each` yields to the scheduler so that
other tasks waiting on the same channel get a fair turn. This is the mechanism
behind round-robin fan-out (see Section 5).

### Unbuffered channels

`channel.new()` with no arguments creates a channel with a single-slot
buffer. A send succeeds immediately if the slot is empty, and blocks if it
is full. This gives "at most one value in flight" semantics, which is
equivalent to true rendezvous for most patterns (producer/consumer,
fan-out, pipelines).


## 3. Tasks

### Spawning: `task.spawn(fn)`

```silt
let handle = task.spawn(fn() {
  let result = compute_something()
  channel.send(ch, result)
})
```

`task.spawn` takes a zero-argument function and submits it as a lightweight task
to the thread pool. Spawning is cheap -- it allocates a stack and frames, not an
OS thread. It returns a `Handle` value immediately.

The function is a closure: it captures variables from the surrounding scope.
Since all values in silt are immutable, sharing captured variables between the
spawning task and the spawned task is safe. Under the hood, captured values are
shared via `Rc` (reference counting).

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

### Joining: `task.join(handle)`

```silt
let h = task.spawn(fn() { 42 })
let result = task.join(h)  -- result = 42
```

`task.join` parks the current task until the spawned task completes, then
returns its result -- the value of the last expression in the spawned function's
body. While waiting, the OS thread runs other tasks.

If the spawned task failed with a runtime error, `task.join` propagates the
error.

### Cancelling: `task.cancel(handle)`

```silt
let h = task.spawn(fn() {
  -- long-running work
  42
})
task.cancel(h)
-- h is marked as Cancelled; it will not run further
```

`task.cancel` marks a task as cancelled. The task is removed from the
scheduler's ready queue and will not execute again. Joining a cancelled task
produces an error.

Returns `Unit`.


## 4. Select

### `channel.select(channels)`

`channel.select` lets a task wait on multiple channels at once. It takes a list
of channels and returns a tuple of `(channel, status)` for whichever channel
has data first.

```silt
match channel.select([ch1, ch2, ch3]) {
  (^ch1, Message(val)) -> handle_ch1(val)
  (^ch2, Message(val)) -> handle_ch2(val)
  (^ch3, Message(val)) -> handle_ch3(val)
  (_, Closed)          -> println("all done")
  _                    -> panic("unexpected")
}
```

The return value is a 2-tuple:

- **First element:** the channel that produced the result.
- **Second element:** one of:
  - `Message(value)` -- a value was received from that channel.
  - `Closed` -- all channels in the list are closed and drained.

### The pin operator `^`

The `^` (pin) operator matches against the current value of an existing
variable, rather than creating a new binding. This is how you identify which
channel fired:

```silt
let urgent = channel.new(5)
let normal = channel.new(5)

channel.send(urgent, "alert!")
channel.send(normal, "status ok")

match channel.select([urgent, normal]) {
  (^urgent, Message(msg)) -> println("URGENT: {msg}")
  (^normal, Message(msg)) -> println("normal: {msg}")
  (_, Closed)             -> println("all closed")
  _                       -> println("no message")
}
```

`^urgent` means "match if this is the same channel as the variable `urgent`,"
not "bind a new variable called urgent." The pin operator works in any pattern
position, not just with `channel.select`.

### Wildcard matching

You do not always care which channel fired. Use `_` to match any channel:

```silt
match channel.select([ch1, ch2]) {
  (_, Message(val)) -> println("got {val} from somewhere")
  (_, Closed)       -> println("all done")
}
```

### Variable binding

You can also bind the channel to a new variable to inspect it later:

```silt
match channel.select([ch1, ch2]) {
  (source, Message(val)) -> println("got {val} from channel {source}")
  (_, Closed)            -> println("all done")
}
```

### How select works internally

`channel.select` checks the channels in list order and returns the first one
that has data. If no channel is ready, the task is parked until one of the
channels receives a value (via waker-based notification). When all channels are
closed and empty, it returns `(channel, Closed)`. If no tasks can make progress
and no channels have data, it detects a deadlock and reports an error.

Select only supports receive. You cannot select on send operations.


## 5. Patterns

### Producer/consumer

The fundamental pattern. One task produces data, another consumes it, connected
by a channel.

```
Producer ----> [channel] ----> Consumer
```

```silt
fn main() {
  let ch = channel.new(10)

  let producer = task.spawn(fn() {
    channel.send(ch, "hello")
    channel.send(ch, "from")
    channel.send(ch, "silt")
    channel.close(ch)
  })

  let consumer = task.spawn(fn() {
    channel.each(ch) { msg ->
      println(msg)
    }
  })

  task.join(producer)
  task.join(consumer)
}
-- prints:
--   hello
--   from
--   silt
```

The producer sends values and closes the channel when done. The consumer uses
`channel.each` to drain the channel, which terminates when the channel closes.

### Fan-out / fan-in with workers

Distribute work across multiple tasks, then collect results.

```
              +---> [Worker 1] ---+
              |                   |
[jobs] ------+---> [Worker 2] ---+------> [results]
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
  channel.send(jobs, 40)
  channel.send(jobs, 50)
  channel.send(jobs, 60)
  channel.close(jobs)

  -- Spawn three workers
  let workers = [1, 2, 3] |> list.map { id ->
    task.spawn(fn() {
      channel.each(jobs) { n ->
        channel.send(results, n * 2)
      }
    })
  }

  -- Wait for all workers to finish, then close results
  workers |> list.each { w -> task.join(w) }
  channel.close(results)

  -- Collect
  channel.each(results) { r ->
    println(r)
  }
}
```

When multiple tasks call `channel.each` on the same channel, the scheduler
distributes messages in round-robin order. With three workers and six messages,
each worker processes exactly two: worker 1 gets messages 1 and 4, worker 2
gets 2 and 5, worker 3 gets 3 and 6. This happens because `channel.each`
yields after processing each message, giving the next worker a turn.

### Pipeline processing

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
    channel.close(raw)
  })

  -- Stage 2: double each value
  let s2 = task.spawn(fn() {
    channel.each(raw) { n ->
      channel.send(doubled, n * 2)
    }
    channel.close(doubled)
  })

  -- Stage 3: sum the doubled values
  let s3 = task.spawn(fn() {
    let sum = 0
    channel.each(doubled) { n ->
      -- note: since silt is immutable, you would accumulate
      -- via channel.receive in a loop or use a different pattern
      println("stage 3 got: {n}")
    }
  })

  task.join(s1)
  task.join(s2)
  task.join(s3)
}
-- prints:
--   stage 3 got: 2
--   stage 3 got: 4
--   stage 3 got: 6
```

Each stage only knows about its input and output channels. Adding a stage in
the middle means inserting a new channel and a new task -- nothing else changes.

### Multiplexing with select

Use `channel.select` to merge multiple input streams.

```silt
fn main() {
  let urgent = channel.new(5)
  let normal = channel.new(5)

  task.spawn(fn() {
    channel.send(normal, "background task done")
    channel.send(normal, "log rotation complete")
    channel.close(normal)
  })

  task.spawn(fn() {
    channel.send(urgent, "disk full!")
    channel.close(urgent)
  })

  -- Process messages from both channels, prioritizing urgent
  -- (select polls in list order, so urgent is checked first)
  let done = false
  loop {
    match channel.select([urgent, normal]) {
      (^urgent, Message(msg)) -> println("URGENT: {msg}")
      (^normal, Message(msg)) -> println("normal: {msg}")
      (_, Closed) -> {
        println("all channels closed")
        return ()
      }
    }
  }
}
```

Since `channel.select` polls channels in list order, putting `urgent` first
gives it priority -- if both channels have data, `urgent` is always checked
first.

### Graceful shutdown via channel.close

Use `channel.close` to signal that a pipeline stage is done. Downstream stages
detect closure via `channel.each` (which terminates) or by matching `Closed`
on `channel.receive`.

```silt
fn main() {
  let work = channel.new(10)
  let done = channel.new(1)

  -- Worker processes until the work channel closes
  let worker = task.spawn(fn() {
    channel.each(work) { item ->
      println("processing: {item}")
    }
    -- channel.each returned, meaning work is closed
    channel.send(done, "shutdown complete")
  })

  -- Send some work, then close
  channel.send(work, "task-a")
  channel.send(work, "task-b")
  channel.send(work, "task-c")
  channel.close(work)

  -- Wait for the worker to finish
  task.join(worker)
  let Message(status) = channel.receive(done)
  println(status)
}
-- prints:
--   processing: task-a
--   processing: task-b
--   processing: task-c
--   shutdown complete
```

This pattern avoids the need for sentinel values or "poison pills." The
channel's closed state is the shutdown signal.

### Spawn, work, join

The simplest pattern: spawn several tasks, let them do work, join them all.

```silt
fn main() {
  let results = channel.new(10)

  let workers = [1, 2, 3] |> list.map { id ->
    task.spawn(fn() {
      channel.send(results, id * 10)
    })
  }

  workers |> list.each { w -> task.join(w) }
  channel.close(results)

  let Message(r1) = channel.receive(results)
  let Message(r2) = channel.receive(results)
  let Message(r3) = channel.receive(results)
  println("results: {r1}, {r2}, {r3}")
  -- output: results: 10, 20, 30
}
```

### Draining with try_receive

When you have finished all producers and want to collect results without
knowing the exact count:

```silt
fn drain(ch) {
  match channel.try_receive(ch) {
    Message(val) -> {
      println("got: {val}")
      drain(ch)
    }
    Empty  -> println("no more data (channel still open)")
    Closed -> println("channel closed, all done")
  }
}

fn main() {
  let ch = channel.new(10)
  channel.send(ch, 1)
  channel.send(ch, 2)
  channel.send(ch, 3)
  channel.close(ch)

  drain(ch)
}
-- prints:
--   got: 1
--   got: 2
--   got: 3
--   channel closed, all done
```


## 6. Runtime Model

### Lightweight tasks on a thread pool

Tasks are lightweight -- each one is just a stack and frames, not an OS thread.
`task.spawn` submits a task to a fixed-size thread pool (sized to the number of
CPU cores). Many tasks are multiplexed onto a smaller number of OS threads, so
you can run thousands of concurrent tasks efficiently.

```
thread pool (N = CPU count)
    ┌──────────────┬──────────────┬──────────────┐
    │  OS thread 1 │  OS thread 2 │  OS thread N │
    │              │              │              │
    │  task A      │  task C      │  task E      │
    │  task B      │  task D      │  task F      │
    │  ...         │  ...         │  ...         │
    └──────────────┴──────────────┴──────────────┘
```

When a task performs a blocking operation (channel receive, select, join), it is
parked -- the OS thread picks up another ready task instead of waiting. When the
blocking condition is satisfied (e.g., a value arrives on the channel), the
parked task is woken and rescheduled.

### Blocking operations

| Operation | Parks when |
|---|---|
| `channel.send(ch, val)` | Buffer is full -- resumes when space opens |
| `channel.receive(ch)` | Buffer is empty -- resumes when a value arrives or the channel closes |
| `task.join(handle)` | Task not yet complete -- resumes when the task finishes |
| `channel.select([...])` | No channel has data -- resumes when any channel becomes ready |

None of these block the OS thread. The task is parked and the thread continues
running other tasks.

### Implications of real parallelism

- **True parallelism.** Multiple tasks execute simultaneously on different CPU
  cores. CPU-bound work benefits from spawning tasks.
- **Lightweight spawning.** Spawning a task allocates a stack, not an OS
  thread. You can have tens of thousands of concurrent tasks.
- **Non-deterministic ordering.** The same inputs may produce different
  interleavings across runs.
- **No data races.** All silt values are immutable, so sending a value
  through a channel is always safe. There is no shared mutable state.
- **No colored functions.** Unlike async/await, there is no function coloring.
  `channel.send` and `channel.receive` look and act like normal function calls.


## 7. Limitations and Future Work

### Current limitations

- **Unbuffered channels are single-slot.** `channel.new()` without an
  argument creates a channel with one-slot buffering rather than true
  rendezvous semantics.

- **No timeouts or timers.** There is no way to say "receive from this channel
  but give up after 5 seconds."

- **Select only supports receive.** `channel.select` polls channels for
  incoming data. You cannot select on send operations.

- **No buffered channel resizing.** A channel's capacity is fixed at creation
  time.

### Future work

- **Timeouts and deadlines.** Adding a timeout parameter to `channel.select`
  or a `channel.receive_timeout`.

- **Buffered send in select.** Extending `channel.select` to support send
  operations alongside receive.


## Quick Reference

| Operation | Syntax | Returns |
|---|---|---|
| Create channel | `channel.new()` / `channel.new(n)` | `Channel` |
| Send (blocking) | `channel.send(ch, val)` | `Unit` |
| Receive (blocking) | `channel.receive(ch)` | `Message(val)` or `Closed` |
| Close | `channel.close(ch)` | `Unit` |
| Try send | `channel.try_send(ch, val)` | `true` or `false` |
| Try receive | `channel.try_receive(ch)` | `Message(val)`, `Empty`, or `Closed` |
| Iterate | `channel.each(ch) { val -> ... }` | `Unit` (when closed) |
| Select | `channel.select([ch1, ch2])` | `(channel, Message(val))` or `(channel, Closed)` |
| Spawn task | `task.spawn(fn() { ... })` | `Handle` |
| Join task | `task.join(handle)` | Task's return value |
| Cancel task | `task.cancel(handle)` | `Unit` |

The mental model: tasks are independent workers, channels are the pipes between
them, `channel.select` is a multiplexer, and `task.join` is a synchronization
barrier. Tasks are lightweight and multiplexed onto a fixed thread pool, giving
you true parallelism without the cost of one OS thread per task.
