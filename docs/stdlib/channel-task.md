---
title: "channel / task"
section: "Standard Library"
order: 12
---

# channel

Bounded channels for concurrent task communication. Channels provide
communication between tasks spawned with `task.spawn`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `close` | `(Channel) -> ()` | Close the channel |
| `each` | `(Channel, (a) -> b) -> ()` | Iterate until channel closes |
| `new` | `(Int?) -> Channel` | Create a channel (0 = rendezvous, N = buffered) |
| `receive` | `(Channel) -> ChannelResult(a)` | Blocking receive |
| `recv_timeout` | `(Channel(a), Duration) -> Result(a, ChannelError)` | Blocking receive with a timeout |
| `select` | `(List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))` | Wait on multiple channels (each op is `Recv(ch)` or `Send(ch, v)`) |
| `send` | `(Channel, a) -> ()` | Blocking send |
| `timeout` | `(Int) -> Channel` | Create a channel that closes after N ms |
| `try_receive` | `(Channel) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel, a) -> Bool` | Non-blocking send |


## `channel.close`

```
channel.close(ch: Channel) -> ()
```

Closes the channel. Subsequent sends will fail. Receivers will see `Closed`
after all buffered messages are consumed.

```silt
import channel
fn main() {
    let ch = channel.new(10)
    channel.send(ch, 1)
    channel.close(ch)
}
```


## `channel.each`

```
channel.each(ch: Channel, f: (a) -> b) -> ()
```

Receives messages from the channel and calls `f` with each one, until the
channel is closed. This is the idiomatic way to consume all messages.

```silt
import channel
import task
fn main() {
    let ch = channel.new(10)
    task.spawn(fn() {
        channel.send(ch, 1)
        channel.send(ch, 2)
        channel.close(ch)
    })
    channel.each(ch) { msg -> println(msg) }
    -- prints 1, then 2
}
```


## `channel.new`

```
channel.new() -> Channel
channel.new(capacity: Int) -> Channel
```

Creates a new channel. With no argument, creates a rendezvous channel
(capacity 0) where the sender blocks until a receiver is ready and vice versa.
With an integer argument, creates a buffered channel with that capacity --
sends block when the buffer is full, receives block when the buffer is empty.

```silt
import channel
fn main() {
    let rendezvous = channel.new()    -- true rendezvous (capacity 0)
    let buffered = channel.new(10)    -- buffered (capacity 10)
}
```


## `channel.receive`

```
channel.receive(ch: Channel) -> ChannelResult(a)
```

Receives a value from the channel. Returns `Message(value)` when a value is
available, or `Closed` when the channel is closed and empty. Parks the task
while waiting, allowing other tasks to run on the same thread.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    match channel.receive(ch) {
        Message(v) -> println(v)
        Closed -> println("done")
        _ -> ()
    }
}
```


## `channel.recv_timeout`

```
channel.recv_timeout(ch: Channel(a), dur: Duration) -> Result(a, ChannelError)
```

Blocking receive with a scoped timeout. Returns:

- `Ok(value)` if a value is delivered within `dur`.
- `Err(ChannelTimeout)` if `dur` elapses with no value and no close.
- `Err(ChannelClosed)` if the channel is closed and has no more buffered values.

A value already buffered, or a rendezvous sender already parked, wins over an
expired timer: the non-blocking path is always tried first so readiness is not
preempted by the timer. A `Duration` of zero gives try-receive semantics (never
schedules a timer); negative durations are a construction error. Positive
sub-millisecond durations are rounded up to one millisecond so the caller
always gets at least one timer tick of wait.

This uses the shared timer thread that backs `channel.timeout` and `time.sleep`
-- no per-call OS thread. Cancelling the surrounding `task.spawn` handle
cleans up both the channel-side waker registration and the timer registration.

`ChannelError` implements the built-in `Error` trait, so `e.message()`
renders either variant as a string:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ChannelTimeout` | — | timer elapsed before a value arrived |
| `ChannelClosed` | — | channel closed with no more values |

```silt
import channel
import task
import time

fn main() {
    let ch = channel.new(0)
    task.spawn(fn() {
        time.sleep(time.ms(50))
        channel.send(ch, 42)
    })
    match channel.recv_timeout(ch, time.ms(500)) {
        Ok(v) -> println(v)                   -- 42
        Err(ChannelTimeout) -> println("timed out")
        Err(ChannelClosed) -> println("channel closed")
    }
}
```


## `channel.select`

```
channel.select(ops: List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))
```

Waits until one of the operations in `ops` can make progress. Every element
is a `ChannelOp(a)` value built with one of two constructors:

- `Recv(ch)` — a **receive** arm that becomes ready when `ch` has a buffered
  value, has a rendezvous sender parked on it, or is closed;
- `Send(ch, value)` — a **send** arm that becomes ready when `ch` has buffer
  capacity or a rendezvous receiver parked on it.

The call returns a 2-tuple of `(channel, result)` identifying the arm that
won and the outcome:

- `(ch, Message(val))` — a `Recv` arm completed with `val`.
- `(ch, Closed)` — a `Recv` arm's channel is closed and drained.
- `(ch, Sent)` — a `Send` arm completed (the value was handed off).

Receive and send arms can be mixed freely in the same call.

Receive-only form:

```silt
import channel
import task
fn main() {
    let ch1 = channel.new(1)
    let ch2 = channel.new(1)
    task.spawn(fn() { channel.send(ch2, "hello") })
    match channel.select([Recv(ch1), Recv(ch2)]) {
        (^ch2, Message(val)) -> println(val)  -- "hello"
        (_, Closed) -> println("closed")
        _ -> ()
    }
}
```

Mixed send and receive — race a pending send against an incoming receive:

```silt
import channel
fn main() {
    let inbox = channel.new(1)
    let outbox = channel.new(1)
    channel.send(inbox, 7)
    match channel.select([Recv(inbox), Send(outbox, 99)]) {
        (^inbox, Message(v)) -> println(v)
        (^outbox, Sent) -> println("sent")
        _ -> ()
    }
}
```


## `channel.send`

```
channel.send(ch: Channel, value: a) -> ()
```

Sends a value into the channel. Parks the task if the buffer is full, allowing
other tasks to run until space opens up.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, "hello")
}
```


## `channel.timeout`

```
channel.timeout(ms: Int) -> Channel
```

Creates a channel that automatically closes after the given number of
milliseconds. The returned channel carries no values -- it simply closes when
the duration elapses. This is useful for adding deadlines to `channel.select`.

```silt
import channel
fn main() {
    let ch = channel.new(10)
    let timer = channel.timeout(1000)  -- closes after 1 second
    match channel.select([Recv(ch), Recv(timer)]) {
        (^ch, Message(val)) -> println("got: {val}")
        (^timer, Closed) -> println("timed out")
        _ -> ()
    }
}
```


## `channel.try_receive`

```
channel.try_receive(ch: Channel) -> ChannelResult(a)
```

Non-blocking receive. Returns `Message(value)` if a value is immediately
available, `Empty` if the channel is open but has no data, or `Closed` if the
channel is closed and empty.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    match channel.try_receive(ch) {
        Message(v) -> println(v)
        Empty -> println("nothing yet")
        Closed -> println("done")
        _ -> ()
    }
}
```


## `channel.try_send`

```
channel.try_send(ch: Channel, value: a) -> Bool
```

Non-blocking send. Returns `true` if the value was successfully buffered,
`false` if the buffer is full or the channel is closed.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    let ok = channel.try_send(ch, 42)
    println(ok)  -- true
}
```


---

# task

Spawn and coordinate lightweight concurrent tasks. Tasks are multiplexed onto a
fixed thread pool and run in parallel. They communicate through channels.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `cancel` | `(Handle) -> ()` | Request cancellation of a task (cooperative; see details below) |
| `deadline` | `(Duration, () -> a) -> a` | Run a callback with a scoped I/O deadline |
| `join` | `(Handle) -> a` | Wait for a task to complete |
| `spawn` | `(() -> a) -> Handle` | Spawn a new lightweight task |
| `spawn_until` | `(Duration, () -> a) -> Handle(a)` | Spawn a task scoped by a deadline |


## `task.cancel`

```
task.cancel(handle: Handle) -> ()
```

Flips the handle's result slot to `Err("cancelled")` using first-writer-wins
semantics: if the task has already completed with some other result,
`task.cancel` is a no-op on the handle. This is **not** a synchronous stop
signal — treat it as a cooperative request, not a hard stop:

- If the task is **currently parked** (blocked on a channel, `task.join`,
  `time.sleep`, or a timer), the pending wake registrations are torn down and
  the task will not be resumed. The handle resolves to `Err("cancelled")`.
- If the task is **currently running**, the handle's result is set
  immediately, but the running slice continues executing until its next
  cooperative yield point or natural completion. Any side effects the slice
  performs before it next parks — writes, spawns, channel sends, I/O — run to
  completion. Its own final result is then discarded (first-writer-wins).

`task.join` on a cancelled handle does **not** return `Err("cancelled")`
as a value — it raises the failure as a runtime error of the form
`joined task failed: cancelled`. Silt has no `try`/`catch`, so when
cancellation is an expected outcome you typically either (a) signal
completion through a sentinel channel and wait on that instead of joining,
or (b) scope the join to a boundary where the raised error is the expected
exit path. See
[Concurrency: Cancelling](../concurrency.md#cancelling-taskcancelhandle) for
the canonical treatment.

```silt
-- noexec
import channel
import task
fn main() {
    let done = channel.new(1)
    let h = task.spawn(fn() {
        -- long-running work
        channel.send(done, 42)
    })
    task.cancel(h)
    -- `task.join(h)` here would raise `joined task failed: cancelled`.
    -- Use the sentinel channel for a non-raising "settled" signal, or
    -- only call `task.join` at a boundary that tolerates the raise.
}
```


## `task.join`

```
task.join(handle: Handle) -> a  -- raises on failure
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

If the joined task panicked, errored, or was cancelled, `task.join` does
**not** surface the failure as an `Err` value. It raises a runtime error of
the form `joined task failed: <msg>` at the call site (e.g. `joined task
failed: cancelled` for a cancelled handle, `joined task failed: division by
zero` for a panicking task body). Silt has no `try`/`catch`, so a joined
failure is terminal for the joining task — when cancellation or task
failure is an expected outcome, use a channel handshake or sentinel value
instead of relying on `task.join` for the signal.

```silt
import task
fn main() {
    let h = task.spawn(fn() { 1 + 2 })
    let sum = task.join(h)
    println(sum)  -- 3
}
```


## `task.spawn`

```
task.spawn(f: () -> a) -> Handle
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join` or `task.cancel`.

```silt
import task
fn main() {
    let h = task.spawn(fn() {
        println("running in a task")
        42
    })
    let answer = task.join(h)
    println(answer)  -- 42
}
```


## `task.deadline`

```
task.deadline(dur: Duration, f: () -> a) -> a
```

Runs `f` with a scoped I/O deadline. If any blocking I/O builtin inside `f`
(see [Concurrency: Blocking operations](../concurrency.md#blocking-operations))
runs longer than `dur`, the builtin returns **the module's own typed
timeout variant** instead of its normal result — the surrounding silt code
handles it through the usual `Result` match on the typed `IoError`,
`TcpError`, or `HttpError` enum that builtin already declares. No exception
is raised, and the deadline does not preempt pure CPU work; it only applies
to I/O.

Specifically (matching the [`SILT_IO_TIMEOUT`](../concurrency.md#io-timeouts-silt_io_timeout)
table):

- `io.*` and `fs.*` surface `Err(IoUnknown("I/O timeout (task.deadline exceeded)"))`.
- `tcp.*` surfaces `Err(TcpTimeout)`.
- `http.*` surfaces `Err(HttpTimeout)`.

The deadline is *scoped*: it nests cleanly with an outer `SILT_IO_TIMEOUT`
or a surrounding `task.deadline`, whichever elapses first fires. The
embedded message on the `IoUnknown` variant distinguishes the source so
silt code can tell scoped timeouts from the global one.

```silt
-- noexec
import io
import task
import time

fn main() {
    let outcome = task.deadline(time.ms(200), fn() {
        io.read_file("/var/log/slow.log")
    })
    match outcome {
        Ok(contents) -> println(contents)
        Err(IoUnknown(msg)) -> println(msg)  -- "I/O timeout (task.deadline exceeded)"
        Err(_) -> println("other io error")
    }
}
```


## `task.spawn_until`

```
task.spawn_until(dur: Duration, f: () -> a) -> Handle(a)
```

Spawns `f` as a task with a bounded wall-clock deadline. Equivalent to
`task.spawn(fn() { task.deadline(dur, f) })` but with one less closure
wrapper. The returned handle resolves to the function's result if it
finishes in time, or to the deadline error inside any I/O builtin it
was blocked on when the deadline fired.

Useful for fan-out patterns where each child task must bound its own
runtime -- e.g. racing N replicas and dropping stragglers.

```silt
-- noexec
import io
import task
import time

fn main() {
    let h = task.spawn_until(time.seconds(2), fn() {
        io.read_file("/tmp/maybe_slow.txt")
    })
    match task.join(h) {
        Ok(contents) -> println(contents)
        Err(msg) -> println(msg)
    }
}
```
