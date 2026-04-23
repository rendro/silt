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
| `select` | `(List(Channel(a) \| (Channel(a), a))) -> (Channel(a), ChannelResult(a))` | Wait on multiple channels (receive and/or send arms) |
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
channel.select(ops: List(Channel(a) | (Channel(a), a))) -> (Channel(a), ChannelResult(a))
```

Waits until one of the operations in `ops` can make progress. Each element of
the list is either:

- a bare `Channel(a)` — a **receive** arm that becomes ready when the channel
  has a buffered value, a rendezvous sender parked on it, or has been closed;
- a `(Channel(a), value)` tuple — a **send** arm that becomes ready when the
  channel has buffer capacity or a rendezvous receiver parked on it.

The call returns a 2-tuple of `(channel, result)` identifying the arm that
won and the outcome:

- `(ch, Message(val))` — a receive arm completed with `val`.
- `(ch, Closed)` — a receive arm's channel is closed and drained.
- `(ch, Sent)` — a send arm completed (the value was handed off).

Receive and send arms can be mixed freely in the same call.

Receive-only form:

```silt
import channel
import task
fn main() {
    let ch1 = channel.new(1)
    let ch2 = channel.new(1)
    task.spawn(fn() { channel.send(ch2, "hello") })
    match channel.select([ch1, ch2]) {
        (^ch2, Message(val)) -> println(val)  -- "hello"
        (_, Closed) -> println("closed")
        _ -> ()
    }
}
```

Send-form sketch — the list element `(out, 42)` is a send arm; when the
arm fires, the match result is `(^out, Sent)`:

```
match channel.select([(out, 42)]) {
    (^out, Sent)        -> ...  -- the send landed
    _                   -> ...
}
```

Send and receive arms can be mixed in the same list so a task can race a
send against a receive and commit to whichever becomes ready first.


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
    match channel.select([ch, timer]) {
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
| `cancel` | `(Handle) -> ()` | Cancel a running task |
| `deadline` | `(Duration, () -> a) -> a` | Run a callback with a scoped I/O deadline |
| `join` | `(Handle) -> a` | Wait for a task to complete |
| `spawn` | `(() -> a) -> Handle` | Spawn a new lightweight task |
| `spawn_until` | `(Duration, () -> a) -> Handle(a)` | Spawn a task scoped by a deadline |


## `task.cancel`

```
task.cancel(handle: Handle) -> ()
```

Cancels a running task. The task will not execute further. No-op if the task has
already completed.

```silt
import task
fn main() {
    let h = task.spawn(fn() {
        -- long-running work
    })
    task.cancel(h)
}
```


## `task.join`

```
task.join(handle: Handle) -> a
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

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
runs longer than `dur`, the builtin returns `Err("I/O timeout (task.deadline
exceeded)")` instead of its normal result -- the surrounding silt code
handles it through the usual `Result` match. No exception is raised, and the
deadline does not preempt pure CPU work; it only applies to I/O.

The deadline is *scoped*: it nests cleanly with an outer `SILT_IO_TIMEOUT`
or a surrounding `task.deadline`, whichever elapses first fires. The error
message distinguishes the source so silt code can tell scoped timeouts from
the global one.

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
        Err(msg) -> println(msg)  -- "I/O timeout (task.deadline exceeded)"
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
