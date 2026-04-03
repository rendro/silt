# Concurrency

Silt provides built-in concurrency based on CSP (Communicating Sequential
Processes). Tasks communicate through channels. There is no shared mutable
state -- every value in silt is immutable, so sending a value through a channel
is always safe. The current runtime is single-threaded and cooperatively
scheduled; the CSP API is designed to be forward-compatible with a future
preemptive runtime.

All concurrency primitives live in two modules: `channel` and `task`. There are
no concurrency keywords.

-----

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
| **CSP (silt)** | Independent tasks, typed channels, `channel.select` | No shared state (by design), cooperative scheduling |

Unlike Go's goroutines, which are multiplexed onto OS threads by a preemptive
runtime, silt tasks are currently coroutines on a single thread. They yield at channel
operations and `task.join`. The CSP API is forward-compatible with a future
preemptive runtime -- user code would not need to change.

-----

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
the current task blocks: the scheduler runs other pending tasks to drain the
channel, then retries. If the channel is closed, sending is an error.

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

If the buffer is empty but the channel is still open, the current task blocks:
the scheduler runs other pending tasks to produce a value, then retries.

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
`list.each`.

After processing each message, `channel.each` yields to the scheduler so that
other tasks blocked on the same channel get a fair turn. This is the mechanism
behind round-robin fan-out (see Section 5).

### Unbuffered channel implementation

`channel.new()` creates a channel with capacity 0, but the runtime promotes
this to capacity 1 internally. Because the cooperative scheduler cannot park a
sender mid-expression, a truly zero-capacity rendezvous is not possible. In
practice, a send on an "unbuffered" channel succeeds immediately if the
single-slot buffer is empty. The sender blocks only if that slot is already
occupied.

This means `channel.new()` gives you "at most one value in flight" rather than
true rendezvous semantics where the sender blocks until a receiver is ready.
For most patterns (producer/consumer, fan-out, pipelines) the difference is
transparent. A future preemptive runtime could implement genuine rendezvous.

-----

## 3. Tasks

### Spawning: `task.spawn(fn)`

```silt
let handle = task.spawn(fn() {
  let result = compute_something()
  channel.send(ch, result)
})
```

`task.spawn` takes a zero-argument function and registers it as a concurrent
task in the scheduler. It returns a `Handle` value immediately -- the task does
not start executing until the scheduler runs it.

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

`task.join` blocks the current task until the spawned task completes, then
returns its result -- the value of the last expression in the spawned function's
body. While waiting, the scheduler runs other pending tasks.

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

-----

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

`channel.select` polls the channels in list order and returns the first one
that has data. If no channel is ready, the scheduler runs pending tasks and
tries again. When all channels are closed and empty, it returns
`(channel, Closed)`. If no tasks can make progress and no channels have data,
it detects a deadlock and reports an error.

Select only supports receive. You cannot select on send operations.

-----

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

-----

## 6. The Runtime

### Single-threaded, cooperative scheduling

Silt runs all tasks on a single OS thread. There is no parallelism. Tasks
are coroutines that yield control at specific points and the scheduler rotates
between them.

This is a deliberate choice: it prioritizes determinism, simplicity, and
debuggability over raw performance. The same inputs always produce the same
task interleaving.

### Yield points

Tasks yield control at these operations:

| Operation | Yields when |
|---|---|
| `channel.send(ch, val)` | Buffer is full -- scheduler runs other tasks to drain it |
| `channel.receive(ch)` | Buffer is empty -- scheduler runs other tasks to fill it |
| `channel.select([...])` | No channel has data -- scheduler runs other tasks |
| `task.join(handle)` | Target task not yet complete -- scheduler runs other tasks |
| `channel.each(ch) { ... }` | After each message -- yields so other tasks get a turn |

Between yield points, a task runs without interruption. There is no preemption
and no time-slicing.

### Task states

Each task is in one of five states:

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

- **Ready** -- eligible to run. The scheduler picks ready tasks in FIFO order.
- **BlockedSend** -- tried to send but the channel buffer is full. Becomes
  Ready when space opens up.
- **BlockedReceive** -- tried to receive but the channel is empty. Becomes
  Ready when a value arrives or the channel closes.
- **Completed** -- finished executing. Its return value is stored in the
  handle.
- **Cancelled** -- cancelled via `task.cancel`. Will not run again.

### How scheduling works

When a blocking operation (send, receive, select, join) cannot proceed:

1. The scheduler takes all Ready tasks from the queue (FIFO order).
2. It evaluates each task's body expression.
3. If a task completes, its result is stored in its handle and it is marked
   Completed.
4. If a task yields (from `channel.each`), it is re-enqueued at the end of the
   ready queue with its updated state.
5. Any still-blocked tasks are returned to the queue.
6. The original operation retries.

This loop repeats until the operation succeeds or the scheduler determines
that no task made progress (deadlock).

### Round-robin fan-out

When multiple tasks compete for the same channel, the scheduler distributes
messages fairly. Two mechanisms make this work:

1. **`channel.receive` yields before attempting to read.** Before trying to
   take a value from the buffer, `channel.receive` runs one round of pending
   tasks. This gives other tasks a chance to receive first.

2. **`channel.each` yields after each message.** After calling the callback
   with a received value, `channel.each` yields the current task back to the
   scheduler. The next task in the queue gets a turn.

The result is round-robin distribution. If three workers loop on
`channel.each(jobs)` and six messages are sent, each worker gets exactly two
messages: worker 1 gets messages 1 and 4, worker 2 gets 2 and 5, worker 3
gets 3 and 6.

### Deadlock detection

The scheduler detects deadlocks when no task can make progress:

- `channel.send` finds the buffer full, but no tasks can drain it.
- `channel.receive` finds the buffer empty, but no tasks can fill it.
- `task.join` is waiting for a task that is not making progress.
- `channel.select` finds all channels empty and no tasks can produce data.

In each case, silt reports a clear error message rather than hanging:

```
deadlock: channel 0 is full and no task can drain it
deadlock: channel 0 is empty and no task can fill it
join: deadlock detected - target task not completed and no progress
channel.select: deadlock detected - no channels have data and no tasks can make progress
```

### Implications of cooperative scheduling

- **No parallelism.** Only one task executes at a time. Multiple CPU cores are
  not utilized. Spawning tasks does not speed up CPU-bound work.
- **CPU-bound tasks block everything.** A task that does a long computation
  without touching a channel will not yield, starving all other tasks.
- **Deterministic ordering.** The same inputs produce the same interleaving
  every time. This makes concurrent programs reproducible and easy to test.
- **No colored functions.** Unlike async/await, there is no function coloring.
  Any function can be called from any context. `channel.send` and
  `channel.receive` look and act like normal function calls.

-----

## 7. Limitations and Future Work

### Current limitations

- **No true parallelism.** Tasks interleave on a single OS thread. CPU-bound
  work gets no speedup from spawning tasks.

- **Unbuffered channels are capacity-1.** `channel.new()` is internally
  promoted to a single-slot buffer. A send succeeds immediately if the slot
  is empty, which differs from true rendezvous semantics where the sender
  blocks until a receiver is ready.

- **No timeouts or timers.** There is no way to say "receive from this channel
  but give up after 5 seconds." A `channel.receive` on an empty channel with
  no producer will deadlock (detected and reported, but not recoverable).

- **Select only supports receive.** `channel.select` polls channels for
  incoming data. You cannot select on send operations.

- **No buffered channel resizing.** A channel's capacity is fixed at creation
  time.

- **CPU-bound tasks starve the scheduler.** A task in a tight loop that never
  touches a channel will hold the thread indefinitely. Other tasks cannot
  make progress until it yields.

### Forward compatibility

The CSP API (`channel.new`, `channel.send`, `channel.receive`, `task.spawn`,
`channel.select`) is designed to be runtime-agnostic. Code written against this
API today will work without modification on a future preemptive runtime. Only
the scheduler implementation would change.

### Future work

- **Preemptive runtime.** Running tasks on a real async runtime or OS threads
  would enable true parallelism. Silt's full immutability makes this safe by
  default -- no data races, no locking needed.

- **True unbuffered channels.** A preemptive scheduler would allow genuine
  rendezvous semantics where the sender parks until a receiver is ready.

- **Timeouts and deadlines.** Adding a timeout parameter to `channel.select`
  or a `channel.receive_timeout` would enable patterns like "wait for a
  response, but give up after 100ms."

- **Buffered send in select.** Extending `channel.select` to support send
  operations alongside receive.

- **Work-stealing scheduler.** A multi-threaded scheduler where idle threads
  steal tasks from busy threads, enabling better utilization of CPU cores.

-----

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
barrier. Currently, all of this runs cooperatively on a single thread -- tasks
interleave at channel operations but never execute in parallel.
