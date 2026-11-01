---
title: "channel / task"
section: "Standard Library"
order: 13
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
| `receive` | `(Channel(a)) -> Option(a)` | Blocking receive: `None` once the channel is closed and empty |
| `receive_timeout` | `(Channel(a), Duration) -> ChannelResult(a)` | Receive, waiting no longer than a duration |
| `select` | `(List(ChannelOp(a))) -> Selected(a)` | Wait on multiple channels (each op is `Recv(ch)` or `Send(ch, v)`) |
| `send` | `(Channel(a), a) -> ()` | Blocking send |
| `timeout` | `(Duration) -> Channel(a)` | Create a channel that closes after a duration |
| `try_receive` | `(Channel(a)) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel(a), a) -> Bool` | Non-blocking send |


## `channel.close`

```
channel.close(ch: Channel(a)) -> ()
```

Closes the channel. Subsequent sends will fail. Once all buffered messages
are consumed, `channel.receive` gives `None` (and `channel.try_receive`
`Closed`).

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
  let _rendezvous = channel.new(0) -- true rendezvous (capacity 0)
  let _buffered = channel.new(10) -- buffered (capacity 10)
}
```


## `channel.receive`

```
channel.receive(ch: Channel(a)) -> Option(a)
```

Receives a value from the channel. Returns `Some(value)` when a value is
available, or `None` when the channel is closed and empty. Parks the task
while waiting, allowing other tasks to run on the same thread.

```silt
import channel

fn main() {
  let ch = channel.new(1)
  channel.send(ch, 42)
  match channel.receive(ch) {
    Some(v) -> println(v)
    None -> println("done")
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
preempted by the timer. A `Duration` of zero or less gives try-receive semantics (never
schedules a timer). Positive
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
    channel.Closed -> println("channel closed")
  }
}
```


## `channel.select`

```
channel.select(ops: List(ChannelOp(a))) -> Selected(a)
```

Waits until one of the operations in `ops` can make progress. Every element
is a `ChannelOp(a)` value built with one of two constructors:

- `Recv(ch)` — a **receive** arm that becomes ready when `ch` has a buffered
  value, has a rendezvous sender parked on it, or is closed;
- `Send(ch, value)` — a **send** arm that becomes ready when `ch` has buffer
  capacity or a rendezvous receiver parked on it.

The call returns a `Selected(a)`, which names the channel of the arm that
completed and the outcome:

- `channel.Received(ch, Some(val))` — a `Recv` arm completed with `val`.
- `channel.Received(ch, None)` — a `Recv` arm's channel is closed and drained.
- `channel.Sent(ch)` — a `Send` arm completed (the value was handed off).

A send on a closed channel is a runtime error (`send on closed channel N`),
as in `channel.send` and `channel.try_send`: a select with a `Send` arm on a
closed channel raises it, whatever its other arms could do, and so does one
whose channel is closed while it waits to send.

Receive and send arms can be mixed freely in the same call.

The arms of one call are of one type, `ChannelOp(a)`. To receive from
channels of different types, map each to a channel of one enum and merge
them: see [Receiving from channels of different
types](../concurrency.md#receiving-from-channels-of-different-types).

Receive-only form:

```silt
import channel
import task

fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  let _ = task.spawn { -> channel.send(ch2, "hello") }
  match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
    channel.Received(^ch2, Some(val)) -> println(val) -- hello
    channel.Received(_, None) -> println("closed")
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
    channel.Received(^inbox, Some(v)) -> println(v)
    channel.Sent(^outbox) -> println("sent")
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
duration below a millisecond is one millisecond; a duration of zero or less
closes the channel at once.

```silt
import channel
import time

fn main() {
  let ch = channel.new(10)
  let timer = channel.timeout(time.ms(1000)) -- closes after 1 second
  match channel.select([channel.Recv(ch), channel.Recv(timer)]) {
    channel.Received(^ch, Some(val)) -> println("got: {val}")
    channel.Received(^timer, None) -> println("timed out")
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
| `deadline` | `(Duration, Fn() -> a) -> Result(a, TaskError)` | Run a function, and end it where it waits when the duration has passed |
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
the test that spawned it. The cancel does not wait for the task to stop.
Its effect on the task depends on where the task is at that moment:

- **The task is waiting** (parked in a channel receive or send, a
  `channel.select`, a `task.join`, a `time.sleep`, I/O, or similar): its
  wait ends without taking or sending anything, and the task is ended
  there. A value sent afterwards goes to the next receiver, not to it.
- **The task has never run** (spawned, and not yet picked up by a
  worker): it is dropped without running.
- **The task is not waiting**: it is running, or its time slice ran out
  and it is to run again, or **its wait has just ended** (a sender
  handed it a value, a receiver took the value it waited to send, the
  operation it waited for gave its result) and it has not run again. The
  task goes on with what it holds, for a bounded amount of further
  work. **After a cancel a task takes nothing new; it may still give
  what it holds.** It is ended:
  - at its next operation that would take something, whether or not
    the thing is there: a `channel.receive`, `channel.try_receive` or
    `channel.receive_timeout`, the next message of a `channel.each`,
    the next element of a stream, `tcp.accept` (the connection stays
    with the listener), and a `channel.select` of which no `Send` arm
    can be done at once (its `Recv` arms, timeouts among them, are not
    tried);
  - at its next operation that would have to wait, with nothing sent;
    and at one that would start something which waits for the outside:
    reading or writing a file's contents, the network, a database,
    standard input. Nothing is started: a file it was about to write is
    not written;
  - at a call that would start a task to take in its place:
    `task.deadline` (its function runs as a task of its own) and every
    stream function that starts a stage (`stream.map`, `stream.take`
    and the other transforms, and the sources: `stream.from_list`,
    `stream.tcp_lines`, ...). No task is started, so nothing is taken
    from the channel a stream would have been built over;
  - when it has run 2,000 steps (about 2,000 instructions, a few
    hundred rounds of a small loop), if it reaches none of these
    before. The steps are counted from the first time slice that begins
    after the cancel, so a task that was computing when the cancel came
    is ended at most one time slice and 2,000 steps later.

  What is done in those steps is what gives: a `channel.send` into a
  channel with room or to a receiver that waits, `channel.try_send`, a
  `Send` arm of a `channel.select` that can be done at once,
  `channel.close`; and the `task.join` of a task that has ended. It can
  still start a task with `task.spawn`. A `time.sleep` of no time is
  done, since it waits for nothing; any longer one is a wait. (The `fs`
  functions, which never wait, are done too: `fs.exists`, `fs.remove`,
  `fs.rename` and the others.) The side effects of those steps happen.
  What the task returns or fails with in them changes nothing: the
  handle keeps the cancellation, and a failure is not reported.

  This is the rule of a function that is past its `task.deadline`,
  and the same count: a task that is past its deadline and cancelled has
  2,000 steps in all, not twice that.

**What the further work is for, and what it does not give.** A task that
was just given something has the steps to pass it on by an operation
that does not wait: a worker that receives an item and puts it, or its
result, into a channel with room does not lose the item to a cancel,
and takes no further one. A cancel is still no orderly shutdown. A
worker that receives an item, computes, and then has to *wait* to send
its result (its output channel is full, or is a rendezvous channel
whose receiver is not there yet) is ended in that send, and the item is
gone; so is the item of a worker that computes on it for more than
2,000 steps, or that has to read or write a file for it. Where nothing
may be lost when the work stops, tell the task to stop over a channel
(a stop message, or the close of its input), let it finish what it has,
and join it.

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
the form `joined task failed: <msg>` in the joining task (e.g. `joined task
failed: cancelled` for a cancelled handle, `joined task failed: division by
zero` for a panicking task body). The error points at where the task failed,
and its call stack goes on, below the task's own calls, with the calls that
led to the `task.join`; the error of a cancelled task points at the join.
Silt has no `try`/`catch`, so a joined
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

Runs `f` and gives `Ok` of what it returns, or `Err(Timeout)` if `f` was
ended because `dur` had passed.

The function runs as a task of its own whose time is over when `dur` has
passed. From then on it takes nothing new and may still give what it
holds, as a task that was cancelled (see [`task.cancel`](#taskcancel)):
it is ended where it would take something or would have to wait,
whatever it waits for (I/O, a `time.sleep`, a `channel.receive` or
`channel.send`, a `channel.select`, a `task.join`, a wait deep inside the
callback of a builtin, the read of a stream), and a function that does
neither is ended after a fixed amount of further work.

- A wait of `f` ends one way or the other: it got what it waited for and
  `f` goes on, or the deadline ended it, nothing was received or sent, and
  `f` is ended there. `Err(Timeout)` from a function that is one receive
  means that no value was taken; from one that is one send, that none was
  sent.
- `task.deadline` answers when the function has ended, and says how: it
  returned (`Ok`, also when it got its value just in time and the clock
  has moved on), it was ended (`Err(Timeout)`), or it failed (the runtime
  error is raised by `task.deadline` as it is, whatever the clock says).
- When the time is over the function takes nothing new: a receive begun
  then is `Err(Timeout)` whether or not a value is there, and the value
  stays in its channel; no I/O, no stream stage and no inner
  `task.deadline` is started. (The function's time is over from the
  first time slice that begins after it; in the slice in which it
  passes, a receive of a value that is there may still complete.) It may
  still give what it holds: a send
  that can be done at once is done. A `dur` of zero or less is a time
  that is over from the start: `task.deadline(time.ms(0), { -> 1 })` is
  `Ok(1)`, and a function that receives is `Err(Timeout)`.
- The work past the time is bounded in all: a function whose time is over
  runs on for at most 2,000 steps (about 2,000 instructions, a few hundred
  rounds of a small loop), whatever it does with them. Then it is ended,
  with what it holds: after the step that used them up, or at the next
  wait it begins. One call of a builtin that is under way runs to its end
  first.
- An I/O operation in flight is given up (a connection it was reading or
  writing is shut down); what had started may have happened all the same.
  A connection that reaches a `tcp.accept` as it is given up stays with
  the listener for the next accept.
- Deadlines nest: an inner function has the outer time too, if that is
  earlier. When the outer time passes first, the outer `task.deadline`
  gives `Err(Timeout)`.
- A deadline ends its function and nothing else: a task that `f` spawned,
  and a task that `f` was joining, go on. Cancel them with `task.cancel`
  if they are to end too.

See [Deadlines](../concurrency.md#deadlines-taskdeadlinedur-fn) for the
whole rule.

`TaskError` is the module's error enum:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Timeout` | — | the function was ended because its time was over |
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
    Err(e) -> println(Error.message(e))
  }
}
```

A task with a deadline is `task.spawn { -> task.deadline(dur, f) }`: its
handle joins to the same `Result`.


