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
| `close` | `(Channel(a)) -> ()` | Close the channel |
| `each` | `(Channel(a), Fn(a) -> ()) -> ()` | Call a function with each message until the channel closes |
| `new` | `(Int) -> Channel(a)` | Create a channel (0 = rendezvous, N = buffered) |
| `receive` | `(Channel(a)) -> ChannelResult(a)` | Blocking receive |
| `receive_timeout` | `(Channel(a), Duration) -> ChannelResult(a)` | Receive, waiting no longer than a duration |
| `select` | `(List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))` | Wait on multiple channels (each op is `Recv(ch)` or `Send(ch, v)`) |
| `send` | `(Channel(a), a) -> ()` | Blocking send |
| `timeout` | `(Duration) -> Channel(a)` | Create a channel that closes after a duration |
| `try_receive` | `(Channel(a)) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel(a), a) -> Bool` | Non-blocking send |


## `channel.close`

```
channel.close(ch: Channel(a)) -> ()
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
channel.each(ch: Channel(a), f: Fn(a) -> ()) -> ()
```

Receives messages from the channel and calls `f` with each one, until the
channel is closed. This is the idiomatic way to consume all messages. Like
the `each` of a list, a map or a set, it takes a function that returns `()`:
a value the function would compute is used inside it or discarded there
(`let _ = ...`).

```silt
import channel
import task

fn main() {
  let ch = channel.new(10)
  let _ = task.spawn { ->
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.close(ch)
  }
  channel.each(ch) { msg -> println(msg) }
  -- prints 1, then 2
}
```


## `channel.new`

```
channel.new(capacity: Int) -> Channel(a)
```

Creates a new channel. With capacity 0 it is a rendezvous channel: the sender
blocks until a receiver is ready and vice versa. With a larger capacity it is
buffered: sends block when the buffer is full, receives block when the buffer
is empty. A negative capacity is an error.

```silt
import channel

fn main() {
  let rendezvous = channel.new(0) -- true rendezvous (capacity 0)
  let buffered = channel.new(10) -- buffered (capacity 10)
}
```


## `channel.receive`

```
channel.receive(ch: Channel(a)) -> ChannelResult(a)
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
    channel.Message(v) -> println(v)
    channel.Closed -> println("done")
    _ -> ()
  }
}
```


## `channel.receive_timeout`

```
channel.receive_timeout(ch: Channel(a), dur: Duration) -> ChannelResult(a)
```

Blocking receive that waits no longer than `dur`. Returns:

- `Message(value)` if a value is delivered within `dur`.
- `Empty` if `dur` elapses with no value and no close: what
  `channel.try_receive` gives when nothing is there now.
- `Closed` if the channel is closed and has no more buffered values.

A value already buffered, or a rendezvous sender already parked, wins over an
expired timer: the non-blocking path is always tried first so readiness is not
preempted by the timer. A `Duration` of zero gives try-receive semantics (never
schedules a timer); negative durations are a construction error. Positive
sub-millisecond durations are rounded up to one millisecond so the caller
always gets at least one timer tick of wait.

This uses the shared timer thread that backs `channel.timeout` and `time.sleep`
-- no per-call OS thread. Cancelling the surrounding `task.spawn` handle
takes the task off the channel and out of the timer.

```silt
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  let _ = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(ch, 42)
  }
  match channel.receive_timeout(ch, time.ms(500)) {
    channel.Message(v) -> println(v) -- 42
    channel.Empty -> println("timed out")
    _ -> println("channel closed")
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

A send on a closed channel is a runtime error (`send on closed channel N`),
as in `channel.send` and `channel.try_send`: a select with a `Send` arm on a
closed channel raises it, whatever its other arms could do, and so does one
whose channel is closed while it waits to send.

Receive and send arms can be mixed freely in the same call.

Receive-only form:

```silt
import channel
import task

fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let _ = task.spawn { -> channel.send(ch2, "hello") }
  match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
    (^ch2, channel.Message(val)) -> println(val) -- hello
    (_, channel.Closed) -> println("closed")
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
  match channel.select([channel.Recv(inbox), channel.Send(outbox, 99)]) {
    (^inbox, channel.Message(v)) -> println(v)
    (^outbox, channel.Sent) -> println("sent")
    _ -> ()
  }
}
```


## `channel.send`

```
channel.send(ch: Channel(a), value: a) -> ()
```

Sends a value into the channel. Parks the task if the buffer is full, allowing
other tasks to run until space opens up. A send on a closed channel is a
runtime error (`send on closed channel N`), also for a send that was waiting
when the channel was closed, as in `channel.try_send` and in a `Send` arm of
`channel.select`.

```silt
import channel

fn main() {
  let ch = channel.new(1)
  channel.send(ch, "hello")
}
```


## `channel.timeout`

```
channel.timeout(dur: Duration) -> Channel(a)
```

Creates a channel that automatically closes after the given `Duration`. The
returned channel carries no values -- it simply closes when the duration
elapses. This is useful for adding deadlines to `channel.select`. A positive
duration below a millisecond is one millisecond; a negative one is an error.

```silt
import channel
import time

fn main() {
  let ch = channel.new(10)
  let timer = channel.timeout(time.ms(1000)) -- closes after 1 second
  match channel.select([channel.Recv(ch), channel.Recv(timer)]) {
    (^ch, channel.Message(val)) -> println("got: {val}")
    (^timer, channel.Closed) -> println("timed out")
    _ -> ()
  }
}
```


## `channel.try_receive`

```
channel.try_receive(ch: Channel(a)) -> ChannelResult(a)
```

Non-blocking receive. Returns `Message(value)` if a value is immediately
available (in the buffer, or held by a sender that waits on a rendezvous
channel), `Empty` if the channel is open but has no data, or `Closed` if the
channel is closed and empty.

```silt
import channel

fn main() {
  let ch = channel.new(1)
  match channel.try_receive(ch) {
    channel.Message(v) -> println(v)
    channel.Empty -> println("nothing yet")
    channel.Closed -> println("done")
    _ -> ()
  }
}
```


## `channel.try_send`

```
channel.try_send(ch: Channel(a), value: a) -> Bool
```

Non-blocking send. Returns `true` if the value was buffered or handed to a
receiver that waits, `false` if there is no room for it now. A send on a
closed channel is a runtime error (`send on closed channel N`), as in
`channel.send` and in a `Send` arm of `channel.select`.

```silt
import channel

fn main() {
  let ch = channel.new(1)
  let ok = channel.try_send(ch, 42)
  println(ok) -- true
}
```


---

# task

Spawn and coordinate lightweight concurrent tasks. Tasks are multiplexed onto a
fixed thread pool and run in parallel. They communicate through channels.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `cancel` | `(Handle(a)) -> ()` | Request cancellation of a task (cooperative; see details below) |
| `deadline` | `(Duration, Fn() -> a) -> Result(a, TaskError)` | Run a function, and end it if it has not returned when the duration has passed |
| `join` | `(Handle(a)) -> a` | Wait for a task to complete |
| `spawn` | `(Fn() -> a) -> Handle(a)` | Spawn a new lightweight task |


## `task.cancel`

```
task.cancel(handle: Handle(a)) -> ()
```

Flips the handle's result slot to `Err("cancelled")` using first-writer-wins
semantics: if the task has already completed with some other result,
the result is kept: a later `task.join` still returns it, or raises it if the
task failed. Cancelling a
task that has already failed also dismisses that failure: it is not
reported as unjoined, does not make `silt run` exit 1 and does not fail
the test that spawned it. The cancel does not wait for the task to stop:

- If the task is **currently parked** (blocked on a channel, a select,
  `task.join`, `time.sleep`, or I/O), its wait ends without taking or
  sending anything, and the task is dropped.
- If the task is **currently running**, it runs no further than the end of
  its current slice (a few thousand steps); side effects of that slice still
  happen. If the slice reaches a blocking operation, the task ends there
  instead of waiting.
- If the task is **queued but not yet running**, it is dropped without
  running again.

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
  let h = task.spawn { ->
    -- long-running work
    channel.send(done, 42)
  }
  task.cancel(h)
  -- `task.join(h)` here would raise `joined task failed: cancelled`.
  -- Use the sentinel channel for a non-raising "settled" signal, or
  -- only call `task.join` at a boundary that tolerates the raise.
}
```


## `task.join`

```
task.join(handle: Handle(a)) -> a
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
  let h = task.spawn { -> 1 + 2 }
  let sum = task.join(h)
  println(sum) -- 3
}
```


## `task.spawn`

```
task.spawn(f: Fn() -> a) -> Handle(a)
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join` or `task.cancel`.

```silt
import task

fn main() {
  let h = task.spawn { ->
    println("running in a task")
    42
  }
  let answer = task.join(h)
  println(answer) -- 42
}
```


## `task.deadline`

```
task.deadline(dur: Duration, f: Fn() -> a) -> Result(a, TaskError)
```

Runs `f` and gives `Ok` of what it returns, or `Err(Timeout)` if `f` has not
returned when `dur` has passed, in which case `f` is ended as a cancelled
task is.

The function runs as a task of its own and it is the wait for it that is
bounded, so the rule is the same whatever `f` waits for when its time is up:
I/O, a `time.sleep`, a `channel.receive` or `channel.send`, a `task.join`, a
wait deep inside the callback of a builtin or a stage of a stream.

- An ended function runs no further: a wait of it takes and sends nothing,
  and an I/O operation of it that is in flight is given up (a connection it
  was reading or writing is shut down). A function that only computes is
  ended at the end of the time slice it is in; one call of a builtin that is
  under way runs to its end first.
- A runtime error inside `f` is raised by `task.deadline` as it is.
- Deadlines nest: the one that passes first ends its own function, and an
  outer one ends the inner function with it.
- A negative `dur` is a runtime error.

`TaskError` is the module's error enum:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Timeout` | — | the function had not returned when its deadline passed |
| `Other(String)` | message | unclassified failure |

```silt
-- noexec
import io
import task
import time

fn main() {
  let outcome = task.deadline(time.ms(200)) { -> io.read_file("/var/log/slow.log") }
  match outcome {
    Ok(Ok(contents)) -> println(contents)
    Ok(Err(e)) -> println("the read failed: {e}")
    Err(task.Timeout) -> println("the read took too long")
    Err(e) -> println(e.message())
  }
}
```

A task with a deadline is `task.spawn { -> task.deadline(dur, f) }`: its
handle joins to the same `Result`.


