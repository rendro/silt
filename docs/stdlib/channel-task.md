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

A signal that carries nothing needs no type of its own. To wait for
work or a stop, give the stop channel the type of the work channel,
send nothing on it, and close it to signal: a closed channel is
received as `None`, so the `None` of the stop channel is the stop.

```silt
import channel
import task

fn work(jobs: Channel(Int), quit: Channel(Int), done: Int) -> Int {
  match channel.select([channel.Recv(jobs), channel.Recv(quit)]) {
    channel.Received(^jobs, Some(job)) -> work(jobs, quit, done + job)
    channel.Received(^quit, None) -> done
    _ -> done
  }
}

fn main() {
  let jobs: Channel(Int) = channel.new(0)
  let quit: Channel(Int) = channel.new(0)
  let worker = task.spawn { -> work(jobs, quit, 0) }
  channel.send(jobs, 1)
  channel.send(jobs, 2)
  channel.close(quit)
  println(task.join_handle(worker)) -- 3
}
```

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
| `cancel` | `(Handle(a)) -> ()` | Cancel a task, and wait until it has ended (see details below) |
| `deadline` | `(Duration, Fn() -> a) -> Option(a)` | Run a function, and end it when the duration has passed: Some of its value, or None |
| `guard` | `(Fn() -> a) -> Result(a, RuntimeError)` | Run a function, and give a runtime error it raises as an `Err` |
| `join_handle` | `(Handle(a)) -> a` | Wait for a task to complete |
| `spawn` | `(Fn() -> a) -> Handle(a)` | Spawn a new lightweight task |


## `task.cancel`

```
task.cancel(handle: Handle(a)) -> ()
```

It also cancels every task that the task spawned and that still runs,
and every one that those spawned, also when the task has already
returned; a task that has ended keeps its result, and one spawned by a
cancelled task never runs. The tasks the runtime starts in a task's
place (a `task.deadline`'s function, a stream's stages, an
`http.serve`'s connections) count as spawned by it. See
[Concurrency: Cancelling](../concurrency.md#cancelling-taskcancelhandle).

Flips the handle's result slot to `Err("cancelled")` using first-writer-wins
semantics: if the task has already completed with some other result,
the result is kept: a later `task.join_handle` still returns it, or raises it if the
task failed. Cancelling a
task that has already failed also dismisses that failure: it is not
reported as unjoined, does not make `silt run` exit 1 and does not fail
the test that spawned it. A failure the task took with a `task.join_handle` is
not its own: the cancel hands it back to the task that failed, which is
then reported as never joined, unless that task was cancelled too.

`task.cancel` returns when the task has ended, and every task below it
that the cancel reached: their last steps (below) have run. What comes
after the cancel (closing the channel the task sent on, counting what it
sent, removing a file it wrote) sees everything they did. The wait is
short: a cancelled task ends at its next wait or after at most 2,000
steps (a step that is one long call, such as a sort of a long list or a
function of the host, runs to its end first). A task cannot wait for its own end: one that is cancelled itself
(it cancels itself, or a task it was spawned by) does not wait, its call
returns at once, and it has the last steps of a cancelled task (below).

Its effect on the task depends on where the task is at that moment:

- **The task is waiting** (parked in a channel receive or send, a
  `channel.select`, a `task.join_handle`, a `time.sleep`, I/O, or similar): its
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
    with the listener), a `task.join_handle`, also of a task that has ended
    (what that task returned or failed with stays with it: a failure
    of it is reported as never joined), and a `channel.select` of which
    no `Send` arm can be done at once (its `Recv` arms, timeouts among
    them, are not tried);
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
  and `channel.close`. A task it starts with `task.spawn` is cancelled
  at once and never runs. A `time.sleep` of no time is
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

`task.join_handle` on a cancelled handle does **not** return `Err("cancelled")`
as a value — it raises the failure as a runtime error of the form
`joined task failed: cancelled`. A task you cancelled is not joined: the
cancel is the wait for it. See
[Concurrency: Cancelling](../concurrency.md#cancelling-taskcancelhandle) for
an example and how a cancel fits a program.

```silt
-- noexec
import channel
import task

fn main() {
  let results = channel.new(16)
  let h = task.spawn { ->
    -- long-running work that sends what it finds
    channel.send(results, 42)
  }
  task.cancel(h)
  -- The task has ended: closing its channel loses nothing it sent.
  -- `task.join_handle(h)` here would raise `joined task failed: cancelled`.
  channel.close(results)
}
```


## `task.join_handle`

```
task.join_handle(handle: Handle(a)) -> a
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

If the joined task panicked, errored, or was cancelled, `task.join_handle` does
**not** surface the failure as an `Err` value. It raises a runtime error of
the form `joined task failed: <msg>` in the joining task (e.g. `joined task
failed: cancelled` for a cancelled handle, `joined task failed: division by
zero` for a panicking task body). The error points at where the task failed,
and its call stack goes on, below the task's own calls, with the calls that
led to the `task.join_handle`; the error of a cancelled task points at the join.
A failure that went through several joins (a task that joins a task that
failed, and is joined in turn) says `joined task failed:` once; its call
stack shows each task and each join it went through.
A joined failure ends the joining task
unless a [`task.guard`](#taskguard) around the join catches it, which
gives it as an `Err`. Where cancellation is an expected outcome, a
channel handshake or a sentinel value says so more plainly than a failed
join.

```silt
import task

fn main() {
  let h = task.spawn { -> 1 + 2 }
  let sum = task.join_handle(h)
  println(sum) -- 3
}
```


## `task.spawn`

```
task.spawn(f: Fn() -> a) -> Handle(a)
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join_handle` or `task.cancel`.

```silt
import task

fn main() {
  let h = task.spawn { ->
    println("running in a task")
    42
  }
  let answer = task.join_handle(h)
  println(answer) -- 42
}
```


## `task.deadline`

```
task.deadline(limit: Duration, work: Fn() -> a) -> Option(a)
```

Runs `work` and gives `Some` of what it returns, or `None` if `work`
was ended because `limit` had passed.

The function runs as a task of its own, and the deadline is that task's:
when `limit` has passed, its time is over. From then on the function
**takes nothing new; it may still give what it holds**, for a small,
fixed amount of further work. It is ended where it would take something
or would have to wait, and a function that does neither, one that only
computes, is ended after that work. That is one rule for everything
`work` does, whatever it waits
for: I/O (see [Blocking operations](../concurrency.md#blocking-operations)), a
`time.sleep`, a `channel.receive` or `channel.send`, a `channel.select`,
a `task.join_handle`, a wait deep inside the callback of a `list.fold`, or the
read of a stream. A `channel.receive` that nobody will ever answer is no
deadlock inside a deadline: it ends when the deadline has passed. It is
the rule of a task that was cancelled (see
[`task.cancel`](#taskcancel), which lists what takes and what gives).

- **A wait ends one way or the other, never both.** Either the wait got
  what it waited for (the value was received, the send was done, the
  operation gave its result), and the function goes on with it; or the
  deadline ended the wait, nothing was received and nothing was sent, and
  the function is ended there: no further instruction of it runs. So a
  receive under a deadline that gives `None` took no value, and
  a send under one that gives `None` sent none: a loop that tries again
  after a `None` loses nothing and sends nothing twice.
- **`task.deadline` answers when the function has ended**, and says how:
  it returned (`Some`), it was ended by its deadline (`None`), or
  it failed. A function that got its value just in time and returns it is
  `Some`, even though the clock has moved past the deadline meanwhile: the
  answer never contradicts what the function did. It comes at most two
  time slices of the function after the deadline.
- **When the time is over the function takes nothing new.** An
  operation that would take something ends it there, `None`,
  whether or not the thing is there: a `channel.receive`,
  `channel.try_receive` or `channel.receive_timeout`, the next message of
  a `channel.each`, the next element of a stream, `tcp.accept`, a
  `channel.select` of which no `Send` arm can be done at once (its `Recv`
  arms are not tried). So a receive begun when the time is over ends the
  function, every time, and the value stays in its channel. (The
  function's time is over from the first time slice that begins after
  it; in the slice in which it passes, a receive of a value that is
  there may still complete.) Nor does the
  function start anything that would take or wait in its place: an
  operation that waits for the outside (a file's contents, the network,
  a database, standard input) is not started, a stream function that
  would start a stage starts none, and a `task.deadline` it calls ends
  it there, before the inner function is started.
- **It may still give what it holds.** A `channel.send` into a channel
  with room or to a receiver that waits, `channel.try_send`, a `Send`
  arm of a `channel.select` that can be done at once and
  `channel.close` are done; so are the `task.join_handle` of a task that has
  ended and `task.spawn`. That is what a function that got its value
  just in time has to hand it on.
- **A duration of zero or less is a time that is over from the start**,
  and the rule is the same: `task.deadline(time.ms(0), { -> 1 })` is
  `Some(1)`, a function that sends into a channel with room is `Some`, and a
  function that receives is `None` whether the channel is empty
  or not.
- **The work past the time is bounded in all.** A function whose time is
  over runs on for at most 2,000 steps: about 2,000 instructions, a few
  hundred rounds of a small loop. That is what a function which got its
  value just in time has to return it, or to hand it on. Every step
  counts, whatever the function does with it: computing, sends that are
  done at once. When the steps are used up the function is ended: after
  the step that used them up, or at the next wait it begins, of which
  nothing is done then. (The steps are counted from the first time slice
  that begins when the time is over, so a function that computes is
  ended at most 2,000 steps and one time slice past its time. One call
  of a builtin that is under way, the sort of a large list, runs to its
  end first.) So a function that received a value and, instead of
  returning it or handing it on, goes on working past its time is ended
  with the value in hand: a worker that needs more than those steps for
  the item it holds, or has to wait to send it, loses that item, as
  after a cancel.
- **A failure of the function is the caller's**: a runtime error inside
  `work` is raised by `task.deadline` as it is, at the place in `work` where it
  happened, whatever the clock says. A function that fails is never
  reported as timed out.
- **I/O that was given up may have happened.** An I/O operation in flight
  when the function is ended is given up as for a cancelled task (see
  [the I/O pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for)): a
  TCP connection it was reading or writing is shut down, and what the
  read had taken off the wire is gone with it; a file write, an HTTP
  request or a database statement that had started may complete all the
  same, and a line that an `io.read_line` reads after it was given up is
  nobody's. `None` says that the function did not finish, not
  that nothing of it reached the outside. A connection that reaches a
  `tcp.accept` which was given up is not lost: the listener keeps it for
  the next accept.
- **Deadlines nest**: the function of an inner `task.deadline` has the
  outer time too, if that is earlier, and is ended by the same rule. When
  the inner time passes first, the outer function goes on with the inner
  `None` as a value; when the outer time does, both functions are
  over and the outer `task.deadline` gives `None`. The steps
  past that time are one count for the two. (A function whose time is
  already over cannot begin an inner deadline: the call ends it.)
- **A deadline ends its function and what it spawned.** The tasks that
  the function started and that still run, with `task.spawn` or as the
  stages of a stream it built, are cancelled when the function is ended,
  and the tasks those spawned, as `task.cancel` cancels the tasks a task
  spawned (see [`task.cancel`](#taskcancel)). A task the
  function was joining and did not spawn goes on: the join is ended, the
  task is not. A task that goes on and fails later, with nobody left to
  join it, is a failure that nobody joined (see
  [Failures that nobody joins](../concurrency.md#failures-that-nobody-joins)).
- The value of `work` is inside the `Some`, so a function that returns a
  `Result` gives two layers: `Some(Err(io.NotFound(path)))` is a read that
  failed in time, `None` one that did not finish.


See [Deadlines](../concurrency.md#deadlines-taskdeadlinelimit-work) for an
example and how a deadline fits a program.

```silt
-- noexec
import io
import task
import time

fn main() {
  let outcome = task.deadline(time.ms(200)) { -> io.read_file("/var/log/slow.log") }
  match outcome {
    Some(Ok(contents)) -> println(contents)
    Some(Err(e)) -> println("the read failed: {e}")
    None -> println("the read took too long")
  }
}
```

A task with a deadline is `task.spawn { -> task.deadline(limit, work) }`: its
handle joins to the same `Option`.

## `task.guard`

```
task.guard(work: Fn() -> a) -> Result(a, RuntimeError)
```

Runs `work` and gives `Ok` of what it returns, or `Err(e)` if a runtime
error was raised inside it: a `panic`, a division by zero, a failed
assertion of a builtin, raised in `work` or in anything it calls (a
callback of `list.map`, the function of a `task.deadline`, a
`task.join_handle` of a task that failed, a stream stage it reads). The
program goes on after the call.

`work` runs in place, on the task that calls `task.guard`: no task is
started, and a guard that catches nothing costs about what a call costs.
When an error is raised, the calls inside the guard end where they are,
as the error would end them: a stream stage being read is stopped, the
function of a `task.deadline` is ended, a `postgres.transact` rolls
back. Then `task.guard` returns `Err(e)`. Of two guards, one inside the
other, the inner one catches.

These are not runtime errors of the program, and pass through a guard:

- `process.exit`, which ends the program (in a test, it fails the test);
- a stop: a `task.cancel` of the task that runs the guard, or the end of
  a `task.deadline`'s function at its time. The guard does not return,
  and nothing after it runs;
- a deadlock of the program's own thread;
- the step budget an embedder set, used up;
- a fault of silt or of its host: an internal error of the VM, a builtin
  or a host function that panicked, a clock that failed.

`RuntimeError` is a record:

| Field | Type | Meaning |
|-------|------|---------|
| `message` | `String` | the error's text: `division by zero`, `panic: <text>` |
| `location` | `String` | where it was raised, `file:line:col`, as the error's report names the place |
| `trace` | `List(String)` | the calls that led there, innermost first: one entry per line of the report's call stack, `check  at main.silt:9:13` |

Shown, as `println("{e}")` shows it, a `RuntimeError` is the report the
program would have printed for the error: its message, its place with
the line of source, and its call stack. A `RuntimeError` the program
changed (`e.{ message: "..." }`) is shown from its fields, without the
line of source.

```silt
import task

fn half(n: Int) -> Int {
  100 / n
}

fn main() {
  match task.guard({ -> half(0) }) {
    Ok(v) -> println("half: {v}")
    Err(e) -> println(e.message) -- division by zero
  }
  println(task.guard { -> half(4) }) -- Ok(25)
}
```
