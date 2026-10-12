---
title: "Concurrency"
section: "Guide"
order: 3
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
- **Tasks share the environment via `Arc`.** This is safe because everything is
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
-- Rendezvous channel (capacity 0 -- sender blocks until receiver is ready)
let ch = channel.new(0)

-- Buffered channel with capacity 10
let ch = channel.new(10)
```

`channel.new(0)` takes zero or one argument. With no argument, it creates a
rendezvous channel (capacity 0) where the sender blocks until a receiver is
ready and vice versa. With an integer argument, it creates a buffered channel
with that capacity.

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
when let channel.Message(msg) = channel.receive(ch) else {
  return
}
```

`channel.receive` takes one value from the channel's buffer. Its result type
is the shared `ChannelResult(a)` enum, which has four variants:

- `Message(value)` -- a value was available.
- `Closed` -- the channel is closed and the buffer is empty. No more values
  will ever arrive.
- `Empty` -- never produced by blocking `channel.receive` (it parks instead),
  but listed here because the variant is part of the same enum and the
  exhaustiveness checker still requires every arm. Returned by
  `channel.try_receive`.
- `Sent` -- never produced by `channel.receive` either; it confirms a
  completed `Send(...)` arm in `channel.select`. Listed here for the same
  exhaustiveness reason.

Any `match` over `ChannelResult` must cover all four variants (or use a
catch-all arm).

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

- **A send on the closed channel is a runtime error**, `send on closed
  channel N`, whichever function was to send: `channel.send`,
  `channel.try_send`, or a `channel.select` that has a `Send` arm on the
  channel. A send that was waiting when the channel was closed fails the
  same way. So a channel is closed by the side that sends on it, or after
  the senders are done.
- **Receives** drain any remaining buffered values as `Message(value)`. Once
  the buffer is empty, `channel.receive` returns `Closed`.

Returns `Unit`.

### Non-blocking send: `channel.try_send(ch, value)`

```silt
let ch = channel.new(2)
println(channel.try_send(ch, "a")) -- true
println(channel.try_send(ch, "b")) -- true
println(channel.try_send(ch, "c")) -- false (buffer full)
```

Attempts to send without blocking. Returns `true` if the value was placed in
the buffer (or handed to a receiver that waits), or `false` if there is no
room for it now. Never yields to the scheduler. On a closed channel it is the
runtime error of every send on one, not `false`.

### Non-blocking receive: `channel.try_receive(ch)`

```silt
let ch = channel.new(10)
channel.send(ch, 42)

println(channel.try_receive(ch)) -- Message(42)
println(channel.try_receive(ch)) -- Empty
```

Attempts to receive without blocking. Returns a `ChannelResult(a)`. The
variants `try_receive` can produce in practice are:

- `Message(value)` -- a value was available.
- `Empty` -- no data yet, but the channel is still open.
- `Closed` -- no data and the channel is closed.

The fourth `ChannelResult` variant, `Sent`, is reserved for `channel.select`
and is never returned by `try_receive`, but the exhaustiveness checker still
requires every `match` to cover it (or use a catch-all arm).

This lets you distinguish "nothing right now" from "nothing ever again."

On a rendezvous channel, `try_receive` takes the value of a sender that is
parked on it: the sender waits with its value until somebody takes it (see
[Unbuffered channels](#unbuffered-channels-true-rendezvous)).

### Iterating: `channel.each(ch) { val -> ... }`

```silt
let ch = channel.new(10)
channel.send(ch, "hello")
channel.send(ch, "world")
channel.close(ch)

channel.each(ch) { msg -> println("got: {msg}") }
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

### Unbuffered channels (true rendezvous)

`channel.new(0)` with no arguments creates a true rendezvous channel with
capacity 0. The sender blocks until a receiver is ready, and the receiver
blocks until a sender is ready. The value is handed off directly -- it is
never buffered. When `channel.send` returns, the value has been handed to a
receiver that was waiting for it; the receiver may not have run yet. Use a
reply channel if the sender must know that the value was processed.

A sender on a rendezvous channel waits until its value is taken, and
whoever takes it completes the send: a blocking `channel.receive`, a `Recv`
arm of `channel.select`, a `channel.receive_timeout`, or a `channel.try_receive`.
The non-blocking operations do not wait themselves, so they only meet a
counterpart that is already waiting: `channel.try_send` succeeds only while
a receiver is parked on the channel, and `channel.try_receive` gives a value
only while a sender is.

```silt
let ch = channel.new(0) -- capacity 0, true rendezvous

let _ = task.spawn { ->
  -- this blocks until the main task calls channel.receive
  channel.send(ch, "hello")
}

-- this blocks until the spawned task calls channel.send
when let channel.Message(msg) = channel.receive(ch) else {
  return
}
println(msg) -- hello
```

### Timeout channels

`channel.timeout(dur)` creates a channel that automatically closes after the
given `Duration`. It is useful for adding deadlines to `channel.select`
operations.

```silt
let ch = channel.new(10)
let timer = channel.timeout(time.ms(5000)) -- closes after 5 seconds

match channel.select([channel.Recv(ch), channel.Recv(timer)]) {
  (^ch, channel.Message(val)) -> println("got: {val}")
  (^timer, channel.Closed) -> println("timed out after 5s")
  _ -> ()
}
```

The timeout channel carries no values -- it simply closes when the duration
elapses. Receiving from it will block until closure, at which point it
returns `Closed`. For the common case of a *single* receive with a deadline,
prefer `channel.receive_timeout` (next section) -- it does the same race in one
call.

### Receive with timeout: `channel.receive_timeout(ch, dur)`

`channel.receive_timeout` is a blocking receive with a deadline. It is the
direct form of the `channel.timeout` + `channel.select` pattern above when
you only need to wait on one channel:

```silt
let ch = channel.new(10)

let _ = task.spawn { -> channel.send(ch, 42) }

match channel.receive_timeout(ch, time.ms(500)) {
  channel.Message(val) -> println("got: {val}")
  channel.Empty -> println("timed out")
  _ -> println("channel closed")
}
```

Like the other receive builtins, it returns a `ChannelResult(a)`:

- `Message(val)` -- a value was delivered within `dur`.
- `Empty` -- `dur` elapsed with no value and no close: what
  `channel.try_receive` gives when nothing is there now.
- `Closed` -- the channel is closed and its buffer is empty.

A value that is there wins over an expired timer: one in the buffer, or one
held by a sender that waits on a rendezvous channel. The non-blocking path
is always tried first, so a ready value is never preempted by the deadline.
A `Duration` of zero gives try-receive semantics (no timer is scheduled): it
returns such a value, and `Empty` when there is none.
Negative durations are an error. Positive sub-millisecond durations are
rounded up to one millisecond, so the caller always waits at least one timer
tick.

The `dur` argument is a `Duration` built with `time.ms` / `time.seconds`
(the same shape `task.deadline` takes).

Signature: `channel.receive_timeout(ch: Channel(a), dur: Duration) -> ChannelResult(a)`.


## 3. Tasks

### Spawning: `task.spawn(fn)`

```silt
let handle = task.spawn { ->
  let result = compute_something()
  channel.send(ch, result)
}
```

`task.spawn` takes a zero-argument function and submits it as a lightweight task
to the thread pool. Spawning is cheap -- it allocates a stack and frames, not an
OS thread. It returns a `task.Handle` value immediately.

The function is a closure: it captures variables from the surrounding scope.
Since all values in silt are immutable, sharing captured variables between the
spawning task and the spawned task is safe. Under the hood, captured values are
shared via `Arc` (atomic reference counting).

```silt
let multiplier = 10
let ch = channel.new(10)

let h = task.spawn { ->
  -- captures `multiplier` and `ch` from outer scope
  channel.send(ch, multiplier * 2)
}

task.join(h)
when let channel.Message(val) = channel.receive(ch) else {
  return
} -- val = 20
```

### Joining: `task.join(handle)`

```silt
let h = task.spawn { -> 42 }
let result = task.join(h) -- result = 42
```

`task.join` parks the current task until the spawned task completes, then
returns its result -- the value of the last expression in the spawned function's
body. While waiting, the OS thread runs other tasks.

If the spawned task panicked, errored, or was cancelled, `task.join` does
**not** return an `Err` value. It raises the failure as a runtime error of
the form `joined task failed: <msg>` at the call site. Silt has no
`try`/`catch`, so a joined failure is terminal for the joining task — design
cancellation-aware code to signal completion through a channel handshake or
sentinel value when cancellation is an expected outcome rather than an error.

### Cancelling: `task.cancel(handle)`

```silt
let done = channel.new(1)
let h = task.spawn { ->
  -- long-running work
  channel.send(done, 42)
}
task.cancel(h)
-- task.join(h) would raise `joined task failed: cancelled` at this point;
-- instead, wait on the sentinel channel if you want a non-raising
-- "task is settled" signal.
```

`task.cancel(h)` sets the handle's result to `Err("cancelled")` using
first-writer-wins semantics: if the task has already completed with some other
result, `task.cancel` leaves that result on the handle. Cancelling counts as
handling the task, though: if the task had already failed, the cancel
dismisses the failure, so it is not reported as a failure that nobody joined
and does not make the program exit with status 1 (see
[Failures that nobody joins](#failures-that-nobody-joins)). A later
`task.join(h)` still raises the task's own error, because the handle keeps
the result that came first. Its effect on the task
depends on where that task is at the moment of cancellation:

- **Task is currently parked** (blocked on a channel receive/send, a
  `channel.select`, a `task.join`, a `time.sleep`, I/O, or similar): its
  wait ends without taking or sending anything, and the task is dropped.
  A value sent afterwards goes to the next receiver, not to it.
- **Its wait has just ended** (a sender handed it a value, the operation
  it waited for gave its result) and it has not run again: the wait
  stays ended, and the task is dropped with what it was given. The value
  is lost, although its sender was told that it was sent. A cancel does
  not ask whether the task holds something; where that matters, tell the
  task to stop over a channel and let it finish what it has.
- **Task is currently running** (executing a slice between parks): the
  handle's result is set to `Err("cancelled")` immediately, and the task
  runs no further than the end of its current slice (a few thousand
  steps). Side effects of that slice still happen. If the slice reaches a
  blocking operation, the task ends there instead of waiting.
- **Task is queued but not yet running** (spawned but not yet picked up
  by a worker, or woken and waiting for one): it is dropped without
  running again.

In every case a `task.join(h)` on the cancelled handle raises `joined task
failed: cancelled` as a runtime error. This matches the `task.cancel`
section of `docs/stdlib/channel-task.md`; the two are kept in sync
deliberately.

`task.cancel` does not wait for the task to stop: a running slice may still
finish after it returns. Because `task.join` raises on
a cancelled handle (it does not return `Err("cancelled")` as a value), you
typically want one of two patterns when cancellation is an expected outcome:

1. **Sentinel channel handshake** — have the task signal completion
   through a channel before it exits, and wait on that channel instead of
   `task.join`:

   ```silt
   task.cancel(h)
   -- `task.join(h)` here would raise `joined task failed: cancelled`.
   -- If you only need to know the cancel was acknowledged and you do not
   -- need the task's value, omit the join and rely on the sentinel
   -- channel from the task body for your "settled" signal.
   ```

2. **Join at the boundary** — if you do call `task.join` on a cancelled
   handle, accept that it will raise `joined task failed: cancelled` at
   the call site and scope the surrounding code so that failure is the
   expected exit path.

Returns `Unit`.

### When a program ends

A program ends when `main` has returned **and its tasks can do no more**:
every task has ended or waits, and none of them waits for a sleep, a
timeout or an I/O operation that is still pending. Until then `silt run`
keeps going:

- A task that is still at work when `main` returns runs to its end. What it
  prints is part of the program's output; if it fails, the run fails.
- A task that sleeps, or waits for a timeout or for I/O, is waited for.
  A `channel.timeout` channel counts only while a task waits on it: one
  that lost its race in a `select`, or that nobody looks at any more,
  holds nothing up, however long it still has to run.
- A task that waits for something only another task could give (a receive
  on a channel nobody will send on) is dropped when nothing else can run:
  such a wait could never end.
- A task that never stops keeps the program from ending. A ticker, a
  server loop or a watcher has to be told to stop, or cancelled, before
  `main` returns. That holds for a task that waits in I/O as well
  (`tcp.accept`, `io.read_line`): once it is cancelled nobody waits for
  the operation, which then counts for nothing. A server started with
  `http.serve` ends when the task that called it is cancelled.

```silt
import channel
import task
import time

fn main() {
  let ticks = channel.new(8)
  let ticker = task.spawn { ->
    loop {
      time.sleep(time.ms(10))
      let _ = channel.try_send(ticks, ())
      loop()
    }
  }
  let _ = channel.receive(ticks)
  println("one tick")
  -- Without this the program would tick for ever.
  task.cancel(ticker)
}
```

If `main` fails, with a runtime error or by returning `Err`, the program
has failed: nothing is waited for. Under `silt test`, each test ends in the same way, for the
tasks that the test spawned (see
[Testing](language/testing.md#spawned-tasks)).

### Failures that nobody joins

A task that ends with a runtime error (a `panic`, a failed assertion, a
division by zero, ...) hands the error to whoever joins it. If the program
never joins the task and never cancels it, nobody receives the error. Silt
reports such a failure itself when the program ends, and `silt run` then
exits with status 1, even if `main` returned normally:

```silt
import task

fn main() {
  let _ = task.spawn { -> 1 / 0 }
  println("main done")
}
```

```text
main done
error[runtime]: task <handle:0> failed and was never joined: division by zero
 --> main.silt:4:27
   |
 4 |   let _ = task.spawn { -> 1 / 0 }
   |                           ^ task <handle:0> failed and was never joined: division by zero
  = help: join the task with task.join to handle its error, or cancel it with task.cancel
```

The report is rendered like every other runtime error: the file and line
where the task failed, the source line, and the call stack when there is
more than one frame. The rules:

- **A join handles the failure.** `task.join(h)` raises the task's error in
  the joiner (`joined task failed: <msg>`), and the failure is not reported
  a second time.
- **A cancel handles the failure.** `task.cancel(h)` on a task that has
  failed dismisses the failure: no report, no exit status 1.
- **Every failure counts.** The report is made when the program has ended
  (see [When a program ends](#when-a-program-ends)): a task that fails
  after `main` has returned still makes the run fail, every time. The
  example above does not have to wait for its task. (A stage of a stream
  is different: its failure goes to whoever reads the stream, and is not
  reported if nobody does. See
  [stream](stdlib/stream.md#design-notes).)
- **A deadlock shows its cause.** When `main` is told
  `deadlock on main thread`, the failures reported with it are usually the
  reason: a producer that failed before it sent.

Under `silt test`, the failure fails the test that spawned the task (see
[Testing](language/testing.md#spawned-tasks)). In the REPL it is reported
when the session ends, since a later input may still join or cancel the
task; an input is not waited for beyond its own value, so the tasks it
spawned go on while you type the next.

### Deadlines: `task.deadline(dur, fn)`

```silt
let outcome = task.deadline(time.ms(200)) { -> io.read_file("/var/log/slow.log") }
```

`task.deadline(dur, f)` runs `f` and gives `Ok` of what it returns, or
`Err(Timeout)` if `f` was ended because `dur` had passed.

The function runs as a task of its own, and the deadline is that task's:
when `dur` has passed, its time is over. From then on the function is
ended **where it would have to wait**, and a function that never waits is
ended when its time slice has run out. That is one rule for every wait of
`f`, whatever it waits for: I/O (see
[Blocking operations](#blocking-operations)), a `time.sleep`, a
`channel.receive` or `channel.send`, a `channel.select`, a `task.join`, a
wait deep inside the callback of a `list.fold`, or the read of a stream.
A `channel.receive` that nobody will ever answer is no deadlock inside a
deadline: it ends when the deadline has passed.

- **A wait ends one way or the other, never both.** Either the wait got
  what it waited for (the value was received, the send was done, the
  operation gave its result), and the function goes on with it; or the
  deadline ended the wait, nothing was received and nothing was sent, and
  the function is ended there: no further instruction of it runs. So a
  receive under a deadline that answers `Err(Timeout)` took no value, and
  a send that answers `Err(Timeout)` sent none: a loop that tries again
  after a timeout loses nothing and sends nothing twice.
- **`task.deadline` answers when the function has ended**, and says how:
  it returned (`Ok`), it was ended by its deadline (`Err(Timeout)`), or
  it failed. A function that got its value just in time and returns it is
  `Ok`, even though the clock has moved past the deadline meanwhile: the
  answer never contradicts what the function did. It comes at most one
  time slice of the function after the deadline.
- **What is there is not waited for.** An operation that can be done at
  once is done, also when the time is over: a value that lies in a
  channel is received, as by `channel.receive_timeout` with a duration
  that has passed. A duration of zero or less is a time that is over from
  the start, and the rule is the same: `task.deadline(time.ms(0), { -> 1 })`
  is `Ok(1)`, and a function that receives from an empty channel is ended
  there.
- **A function that computes on is ended when its time slice has run
  out**: after 2,000 steps past the last wait that ended for it in time,
  which with the usual time slice is the end of the slice in which its
  time passed. Only one call of a builtin that is under way (the sort of
  a large list) runs to its end first. A function that received a value
  and, instead of returning, goes on computing past its time is ended
  with the value in hand; so is one that has computed for a whole slice
  without waiting and then takes a value that is there. Return what you
  took, or hand it on, before more work.
- **A failure of the function is the caller's**: a runtime error inside
  `f` is raised by `task.deadline` as it is, at the place in `f` where it
  happened, whatever the clock says. A function that fails is never
  reported as timed out.
- **I/O that was given up may have happened.** An I/O operation in flight
  when the function is ended is given up as for a cancelled task (see
  [the I/O pool](#the-io-pool-and-operations-that-nobody-waits-for)): a
  TCP connection it was reading or writing is shut down, and what the
  read had taken off the wire is gone with it; a file write, an HTTP
  request or a database statement that had started may complete all the
  same, and a line that an `io.read_line` reads after it was given up is
  nobody's. `Err(Timeout)` says that the function did not finish, not
  that nothing of it reached the outside. A connection that reaches a
  `tcp.accept` which was given up is not lost: the listener keeps it for
  the next accept.
- **Deadlines nest**: the function of an inner `task.deadline` has the
  outer time too, if that is earlier, and is ended by the same rule. When
  the inner time passes first, the outer function goes on with the inner
  `Err(Timeout)` as a value; when the outer time does, both functions are
  over and the outer `task.deadline` gives `Err(Timeout)`.
- **A deadline ends its function and nothing else.** A task that the
  function started with `task.spawn` goes on when the function is ended,
  and so does a task the function was joining: the join is ended, the
  task is not. Who wants them ended keeps their handles and cancels them
  with `task.cancel`. A task that goes on and fails later, with nobody
  left to join it, is a failure that nobody joined (see
  [Failures that nobody joins](#failures-that-nobody-joins)). The stages
  of a stream the function built go on until nobody reads them.
- The value of `f` is inside the `Ok`, so a function that returns a
  `Result` gives two layers: `Ok(Err(io.NotFound(path)))` is a read that
  failed in time, `Err(task.Timeout)` one that did not finish.

`SILT_IO_TIMEOUT` is another thing: it bounds each single wait for I/O,
for the whole process, and gives the module's own timeout error
(see [I/O timeouts](#io-timeouts-silt_io_timeout)).

```silt
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

A task with a deadline is the two together:
`task.spawn { -> task.deadline(dur, f) }`, whose handle joins to the same
`Result`. That is the shape for fan-out where each child must bound its
own run time, such as racing replicas and dropping the stragglers.

Signature: `task.deadline(dur: Duration, f: Fn() -> a) -> Result(a, task.TaskError)`.


## 4. Select

### `channel.select(ops)`

`channel.select` lets a task wait on multiple channel operations at once. It
takes a list of `ChannelOp(a)` values and returns a tuple of `(channel, status)`
for whichever operation can make progress first. Each op is built with one of
two constructors:

- `Recv(ch)` — a receive arm that becomes ready when `ch` has data, has a
  rendezvous sender parked on it, or is closed.
- `Send(ch, value)` — a send arm that becomes ready when `ch` has buffer
  capacity or a rendezvous receiver parked on it.

```silt
match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
  (^ch1, channel.Message(val)) -> handle_input(val)
  (^ch2, channel.Message(val)) -> handle_other(val)
  (_, channel.Closed) -> println("all done")
  _ -> ()
}
```

The return value is a 2-tuple:

- **First element:** the channel that produced the result.
- **Second element:** one of:
  - `Message(value)` — a `Recv` arm received `value`.
  - `Closed` — a `Recv` arm's channel is closed and drained.
  - `Sent` — a `Send` arm completed (value was handed off).

### The pin operator `^`

The `^` (pin) operator matches against the current value of an existing
variable, rather than creating a new binding. This is how you identify which
channel fired:

```silt
let urgent = channel.new(5)
let normal = channel.new(5)

channel.send(urgent, "alert!")
channel.send(normal, "status ok")

match channel.select([channel.Recv(urgent), channel.Recv(normal)]) {
  (^urgent, channel.Message(msg)) -> println("URGENT: {msg}")
  (^normal, channel.Message(msg)) -> println("normal: {msg}")
  (_, channel.Closed) -> println("all closed")
  _ -> println("no message")
}
```

`^urgent` means "match if this is the same channel as the variable `urgent`,"
not "bind a new variable called urgent." The pin operator works in any pattern
position, not just with `channel.select`.

### Wildcard matching

You do not always care which channel fired. Use `_` to match any channel:

```silt
match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
  (_, channel.Message(val)) -> println("got {val} from somewhere")
  (_, channel.Closed) -> println("all done")
  _ -> ()
}
```

### Variable binding

You can also bind the channel to a new variable to inspect it later. A channel
has no `Display`, so log the payload (or a counter) instead of the
channel itself:

```silt
match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
  (_source, channel.Message(val)) -> println("got {val}")
  (_, channel.Closed) -> println("all done")
  _ -> ()
}
```

### How select works internally

`channel.select` picks a ready branch at random: it sweeps the list starting
from a pseudo-random index, so if multiple channels are ready, any one of them
may win. Readers must not assume that earlier entries in the list have
priority -- the choice is fair, not ordered.

If no branch is ready, the task is parked on all of them at once. Whoever
makes one possible (a sender, a receiver, a close) completes that branch for
the select, and only that one: a select performs exactly one of its
operations. A closed channel is ready for a `Recv`: that branch gives
`(channel, Closed)`. A `Send` on a closed channel is no branch at all: a
select that has one is the runtime error of a send on a closed channel,
whatever its other branches could do, and so is one whose channel is
closed while it waits to send.
A select never meets itself: a `Send` and a `Recv` on the same channel in
one select do not complete each other. If no task can make progress, the
deadlock is reported.

A stream stage counts as a task here. A wait on the output of a pipeline
that can never deliver and never closes (the usual cause is a channel
feeding the pipeline that was never closed with `channel.close`) is a
deadlock like any other, and is reported.


## 5. Patterns

### Producer/consumer

The fundamental pattern. One task produces data, another consumes it, connected
by a channel.

```
Producer ----> [channel] ----> Consumer
```

```silt
import channel
import task

fn main() {
  let ch = channel.new(10)

  let producer = task.spawn { ->
    channel.send(ch, "hello")
    channel.send(ch, "from")
    channel.send(ch, "silt")
    channel.close(ch)
  }

  let consumer = task.spawn { -> channel.each(ch) { msg -> println(msg) } }

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
import channel
import list
import task

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
  let workers = [1, 2, 3]
    |> list.map { id -> task.spawn { -> channel.each(jobs) { n -> channel.send(results, n * 2) } } }

  -- Wait for all workers to finish, then close results
  workers |> list.each { w -> task.join(w) }
  channel.close(results)

  -- Collect
  channel.each(results) { r -> println(r) }
}
```

When multiple tasks call `channel.each` on the same channel, the scheduler
distributes messages fairly across them: every worker gets a turn, no worker
starves. `channel.each` yields after processing each message, so the runtime
can hand the next message to a different waiter.

The distribution is *fair, not ordered*. With three workers and six messages
no worker is guaranteed exactly two -- on any given run the actual split
depends on scheduler timing, and one worker may handle several messages
before another sees one. Treat the workers as an interchangeable pool, not a
deterministic round-robin.

### Pipeline processing

Connect tasks in a linear chain where each stage reads from one channel and
writes to the next.

```
[Stage 1] ----> [ch1] ----> [Stage 2] ----> [ch2] ----> [Stage 3]
```

```silt
import channel
import task

fn main() {
  let raw = channel.new(10)
  let doubled = channel.new(10)

  -- Stage 1: produce raw values
  let s1 = task.spawn { ->
    channel.send(raw, 1)
    channel.send(raw, 2)
    channel.send(raw, 3)
    channel.close(raw)
  }

  -- Stage 2: double each value
  let s2 = task.spawn { ->
    channel.each(raw) { n -> channel.send(doubled, n * 2) }
    channel.close(doubled)
  }

  -- Stage 3: consume the doubled values
  let s3 = task.spawn { -> channel.each(doubled) { n -> println("stage 3 got: {n}") } }

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

Use `channel.select` to merge multiple input streams. When more than one
channel is ready, `select` picks a branch fairly at random -- every ready
branch has an equal chance of being chosen, so you cannot encode priority by
ordering the list.

```silt
import channel
import task

fn main() {
  let alerts = channel.new(5)
  let logs = channel.new(5)

  let _ = task.spawn { ->
    channel.send(logs, "background task done")
    channel.send(logs, "log rotation complete")
    channel.close(logs)
  }

  let _ = task.spawn { ->
    channel.send(alerts, "disk full!")
    channel.close(alerts)
  }

  -- Merge both streams into a single handler. Over many runs, messages from
  -- `alerts` and `logs` interleave fairly -- neither channel starves the other.
  -- `loop _ = () { ... loop(()) }` re-enters explicitly; a bare `loop {}`
  -- without a `loop(...)` recursion runs its body only once.
  loop _ = () {
    match channel.select([channel.Recv(alerts), channel.Recv(logs)]) {
      (^alerts, channel.Message(msg)) -> {
        println("alert: {msg}")
        loop(())
      }
      (^logs, channel.Message(msg)) -> {
        println("log: {msg}")
        loop(())
      }
      (_, channel.Closed) -> {
        println("a channel closed")
        return ()
      }
      _ -> loop(())
    }
  }
}
```

If you genuinely need priority semantics (for example, "always drain `alerts`
before touching `logs`"), check the high-priority channel first with
`channel.try_receive` and fall back to `channel.select` only when it is
empty. `select` itself gives you fairness, not ordering.

### Graceful shutdown via channel.close

Use `channel.close` to signal that a pipeline stage is done. Downstream stages
detect closure via `channel.each` (which terminates) or by matching `Closed`
on `channel.receive`.

```silt
import channel
import task

fn main() {
  let work = channel.new(10)
  let done = channel.new(1)

  -- Worker processes until the work channel closes
  let worker = task.spawn { ->
    channel.each(work) { item -> println("processing: {item}") }
    -- channel.each returned, meaning work is closed
    channel.send(done, "shutdown complete")
  }

  -- Send some work, then close
  channel.send(work, "task-a")
  channel.send(work, "task-b")
  channel.send(work, "task-c")
  channel.close(work)

  -- Wait for the worker to finish
  task.join(worker)
  when let channel.Message(status) = channel.receive(done) else {
    return
  }
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
import channel
import list
import task

fn main() {
  let results = channel.new(10)

  let workers = [1, 2, 3] |> list.map { id -> task.spawn { -> channel.send(results, id * 10) } }

  workers |> list.each { w -> task.join(w) }
  channel.close(results)

  when let channel.Message(r1) = channel.receive(results) else {
    return
  }
  when let channel.Message(r2) = channel.receive(results) else {
    return
  }
  when let channel.Message(r3) = channel.receive(results) else {
    return
  }
  println("results: {r1}, {r2}, {r3}")
  -- output: results: 10, 20, 30 (in some order — the receive order is
  -- a permutation of the spawn order, since the workers race to send)
}
```

### Draining with try_receive

When you have finished all producers and want to collect results without
knowing the exact count:

```silt
import channel

fn drain(ch) {
  match channel.try_receive(ch) {
    channel.Message(val) -> {
      println("got: {val}")
      drain(ch)
    }
    channel.Empty -> println("no more data (channel still open)")
    channel.Closed -> println("channel closed, all done")
    _ -> ()
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

### Supervision and restart

For long-running workers, treat failures as values. Give each worker a
fallible body that returns `Result`, report completion on an outcome
channel, and have a supervisor task decide whether to restart:

```silt
import channel
import task

type Outcome {
  Finished,
  Crashed(String),
}

fn do_job(id, job) {
  when job != "poison" else {
    return Err("worker {id} hit poison pill")
  }
  println("worker {id} handled {job}")
  Ok(())
}

fn worker_body(id, jobs, outcomes) {
  channel.each(jobs) { job ->
    match do_job(id, job) {
      Ok(_) -> ()
      Err(msg) -> {
        channel.send(outcomes, (id, Crashed(msg)))
        return
      }
    }
  }
  channel.send(outcomes, (id, Finished))
}

fn spawn_worker(id, jobs, outcomes) {
  let _ = task.spawn { -> worker_body(id, jobs, outcomes) }
}

fn supervise(jobs, outcomes, outstanding, remaining_restarts) {
  when outstanding > 0 else {
    return
  }
  when let channel.Message((id, outcome)) = channel.receive(outcomes) else {
    return
  }
  match outcome {
    Finished -> supervise(jobs, outcomes, outstanding - 1, remaining_restarts)
    Crashed(msg) -> {
      println("worker {id} failed: {msg}")
      when remaining_restarts > 0 else {
        -- restart budget exhausted: drop this worker without respawning
        supervise(jobs, outcomes, outstanding - 1, remaining_restarts)
        return
      }
      spawn_worker(id, jobs, outcomes)
      -- restart replaces the crashed worker 1-for-1: outstanding unchanged
      supervise(jobs, outcomes, outstanding, remaining_restarts - 1)
    }
  }
}

fn main() {
  let jobs = channel.new(10)
  let outcomes = channel.new(10)

  spawn_worker(1, jobs, outcomes)
  spawn_worker(2, jobs, outcomes)

  -- Two workers are spawned, so two outcomes are expected before the
  -- supervisor returns. Each restart-on-Crashed adds one more outcome
  -- to wait for; the bounded restart budget caps that growth.
  let sup = task.spawn { -> supervise(jobs, outcomes, 2, 3) }

  channel.send(jobs, "a")
  channel.send(jobs, "b")
  channel.close(jobs)
  task.join(sup)
}
```

Key points:

- **Failures are values on a channel.** Workers catch their own errors
  via `Result` and `?`, then send a typed `Outcome` so the supervisor
  can decide policy without relying on panic propagation.
- **Supervisors are just tasks.** There is no built-in supervision
  tree. Compose the policy you need (restart, circuit-break, escalate)
  in ordinary silt — here, bounded restarts via recursion.
- **Cancel children on shutdown.** If the supervisor is cancelled
  (`task.cancel`), cancel each child it spawned to avoid leaks. Track
  handles in a list and cancel them when the outcome channel closes.
- **`task.join` re-raises runtime errors.** If a spawned task's body
  panics with an unhandled runtime error, `task.join` on its handle
  re-raises that error in the joining task. That is the "safety net"
  path — design for outcomes-as-values instead, and keep `task.join`
  for awaiting clean completion.


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

### Scheduler and time slicing

Silt uses a **cooperative** scheduler -- there is no wall-clock preemption.
A task holds its worker thread until one of two things happens:

1. It blocks on an I/O operation, channel send/receive/select, `task.join`,
   or `time.sleep`. The task is parked and the worker moves on to the next
   ready task.
2. It exhausts its **opcode time slice**. The scheduler runs each task for a
   fixed budget of bytecode instructions (default **2000 opcodes**); when the
   budget runs out, the task is re-queued at the back of the run queue and
   the worker picks up whoever is next. This keeps CPU-heavy tasks from
   starving each other.

The slice budget is tunable via the `SILT_TIME_SLICE` environment variable
(for example, `SILT_TIME_SLICE=500 silt run main.silt` for more frequent
rotation, or a larger value to reduce scheduling overhead).

One consequence: a task running a tight CPU loop with no blocking calls will
still yield every ~2000 opcodes, but between those yield points it owns its
worker. If you spawn fewer long CPU-bound tasks than you have workers, the
remaining workers simply stay idle; the scheduler does not migrate a running
task off a busy worker.

### I/O timeouts (`SILT_IO_TIMEOUT`)

Silt's I/O builtins (see the [Blocking operations](#blocking-operations) table
below) normally block indefinitely while they wait on the OS. For deployments
where a stuck syscall or hung remote peer must not freeze a task forever, a
task's wait for an I/O builtin can be given an end.

The `SILT_IO_TIMEOUT` environment variable sets one for every such wait.
Setting `SILT_IO_TIMEOUT=5s` makes every I/O
builtin that blocks longer than that duration return the module's own typed
timeout variant instead of its normal value:

- `io.*` and `fs.*` surface `Err(Other("I/O timeout (SILT_IO_TIMEOUT exceeded)"))`.
- `tcp.*` surfaces `Err(Timeout)`.
- `http.*` surfaces `Err(Timeout)`.

The timeout is *invisible* at the language surface — no new syntax, no
colored functions — because every I/O builtin already returns `Result` with
the matching typed error enum. It is for waits for I/O only: a `time.sleep`
or a wait on a channel is not I/O and is not ended by it.

Accepted format: a suffixed duration only — `5s`, `500ms`, `2m`, `1h`.
A bare integer with no unit (e.g. `SILT_IO_TIMEOUT=5000`) is NOT accepted; it
fails to parse and leaves the waits without an end.

```sh
# Any I/O that stalls > 5s surfaces the module's typed timeout variant.
SILT_IO_TIMEOUT=5s silt run server.silt
```

To bound one piece of work rather than every wait of the process, use
[`task.deadline(dur, fn)`](#deadlines-taskdeadlinedur-fn): it ends the
function it runs, whatever the function waits for, and says so with its own
`Err(Timeout)`. The two are independent: inside a `task.deadline`, a wait
for I/O still ends at `SILT_IO_TIMEOUT` with the module's error if that
comes first.

#### The I/O pool, and operations that nobody waits for

Blocking operations run on threads of their own, the I/O pool, so that
they never hold up the scheduler. The model is plain: **an operation that
blocks has a thread for as long as it blocks**. (silt has no
readiness-based I/O yet; a task that waits in `tcp.read` costs a thread,
about 25 KiB of memory with its task.) The pool grows and shrinks by
itself: an operation gets a thread at once, a new one if all are busy, and
a thread that finds no work for a few seconds ends. Four tasks that each
wait for a line from a silent peer do not delay a file read. There is
nothing to configure.

There is a bound: **4,096 operations in flight**. One more is not queued;
it returns its module's error at once (`Err(Other("too many I/O
operations in flight (4096)"))`, `Err(Other(...))`, ...), because an
operation that waited for a thread might be the very one that would have
released the others. A server that holds more connections than that open
in blocking reads has to refuse some. (`http.serve` has one operation in
flight for its accept, and one for each connection that waits for a
request or sends a response.)

A task stops waiting for its operation when `SILT_IO_TIMEOUT` passes,
when it is cancelled by `task.cancel`, when it is the function of a
`task.deadline` whose time is up, or when it is
dropped at the end of the program. It resumes, or ends, at that moment.
What becomes of the operation depends on what it is:

- **On a TCP connection** (`tcp.read`, `tcp.read_exact`, `tcp.write`, the
  `stream.tcp_*` sources and sink): the connection is **shut down**, as by
  `tcp.close`, which makes the operation return and frees its thread.
  The connection cannot be used afterwards: a read that timed out cannot
  be tried again. Give a read the time the peer may really take, and
  treat a timeout as the end of the connection. An operation whose
  result arrives in the very moment its task stops waiting loses it; the
  connection is shut down all the same.
- **`tcp.accept`**: the accept is woken and gives up; the listener
  stays usable, and a client's connection that reached the accept as it
  was given up is kept by the listener for the next accept (it is closed
  with the listener if none comes). (An accept that waits costs nothing: its thread sleeps
  in the system until a connection comes. To wake it the listener
  connects to its own port on the loopback address; where that is not
  possible, say a packet filter forbids it, the thread stays until a
  client connects.)
- **Everything else** (file I/O, `http.get`, `tcp.connect`,
  `io.read_line`): the operation cannot be interrupted. It runs to its
  end in the background and its result is discarded; a write that had
  started completes. Its thread no longer counts as one of the pool's, so
  operations that never return (a read of a terminal nobody types at) do
  not use the pool up. An operation that had not started yet never runs.
  Such stuck operations have a bound of their own, 64: while that many
  are still running unheard, every new I/O operation fails at once with
  an error that says so, instead of the program taking a thread for each
  without end.

### Blocking operations

| Operation | Parks when |
|---|---|
| `channel.send(ch, val)` | Buffer is full -- resumes when space opens |
| `channel.receive(ch)` | Buffer is empty -- resumes when a value arrives or the channel closes |
| `task.join(handle)` | Task not yet complete -- resumes when the task finishes |
| `channel.select([...ops])` | No `Recv`/`Send` op is ready -- resumes when any becomes ready |
| `time.sleep(duration)` | Always -- parks the task for the given duration, then resumes |
| `io.read_file(path)` | Always (file I/O) |
| `io.write_file(path, content)` | Always (file I/O) |
| `io.read_line()` | Always (stdin) |
| `http.get(url)` | Always (network) |
| `http.request(...)` | Always (network) |

None of these block the OS thread. The task is parked and the thread continues
running other tasks.

I/O operations follow the same transparent yielding pattern as channel
operations -- no special syntax needed. When code calls `io.read_file` or
`http.get`, the operation is dispatched to an I/O pool and the task is
parked until the result is ready. `main` is a task like the others here:
its I/O and its `time.sleep` are waits too.

### Deadlock detection

A program is deadlocked when `main` waits (a `channel.send`,
`channel.receive`, `channel.each`, `channel.select` or `task.join`), every
other task waits too, and nothing is pending that could end one of the
waits: no timer and no I/O operation. silt sees that at the moment it
becomes true -- when the last task parks or ends, or the last timer has
fired -- and the wait of `main` fails with a runtime error that starts with
`deadlock on main thread`; the program exits with status 1. Together with
that error, silt reports the tasks that failed and that nobody joined
(`silt run` before it, `silt test` under the failing test, after it; see
[Failures that nobody joins](#failures-that-nobody-joins)): a failed
producer is the usual reason why a counterpart is missing.

A wait that will end by itself counts as able to go on: a `time.sleep`, a
`channel.receive_timeout` that has not expired, a wait on a `channel.timeout`
channel that has not closed yet (a `channel.timeout` that no task waits on
does not count: it could wake nobody). While a task is in such a wait, no
deadlock is reported, whether or not that task could ever reach the channel
that `main` waits on. The verdict comes when the wait has ended, if the
program is still stuck then. So a background task that sleeps for a minute
delays the report of an unrelated deadlock by up to a minute. The same holds
for a task that waits for an I/O operation. Only `main` gets a deadlock verdict; tasks that are stuck while
`main` is not waiting stay parked.

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
  I/O operations like `io.read_file` and `http.get` also transparently yield
  to the scheduler -- there is no `async io.read_file` or special I/O monad.


## 7. Limitations and Future Work

### Current limitations

- **No buffered channel resizing.** A channel's capacity is fixed at creation
  time.


## Quick Reference

| Operation | Syntax | Returns |
|---|---|---|
| Create channel | `channel.new(0)` / `channel.new(n)` | `Channel` |
| Send (blocking) | `channel.send(ch, val)` | `Unit` |
| Receive (blocking) | `channel.receive(ch)` | `channel.ChannelResult(a)` -- `channel.Message(val)` or `channel.Closed` in practice; `channel.Empty`/`channel.Sent` are unreachable here but still required by exhaustiveness |
| Close | `channel.close(ch)` | `Unit` |
| Try send | `channel.try_send(ch, val)` | `true` or `false` |
| Try receive | `channel.try_receive(ch)` | `channel.ChannelResult(a)` -- `channel.Message(val)`, `channel.Empty`, or `channel.Closed` in practice; `channel.Sent` is unreachable here but still required by exhaustiveness |
| Iterate | `channel.each(ch) { val -> ... }` | `Unit` (when closed) |
| Select | `channel.select([channel.Recv(ch1), channel.Send(ch2, v)])` | `(channel, channel.Message(val))`, `(channel, channel.Closed)`, `(channel, channel.Sent)` |
| Timeout channel | `channel.timeout(dur)` | `Channel` (closes after the `Duration`) |
| Receive with timeout | `channel.receive_timeout(ch, dur)` | `channel.Message(val)`, `channel.Empty` (nothing came in time), or `channel.Closed`; a buffered value wins over an expired timer |
| Spawn task | `task.spawn({ -> ... })` | `task.Handle(a)` |
| Join task | `task.join(handle)` | Task's return value (raises `joined task failed: <msg>` if the task errored or was cancelled) |
| Cancel task | `task.cancel(handle)` | `Unit` (a failure of the task is then not reported) |
| Deadline | `task.deadline(dur, { -> ... })` | `Ok(value)` of the function, or `Err(task.Timeout)` if it was ended because its time was over |

The mental model: tasks are independent workers, channels are the pipes between
them, `channel.select` is a multiplexer, and `task.join` is a synchronization
barrier. Tasks are lightweight and multiplexed onto a fixed thread pool, giving
you true parallelism without the cost of one OS thread per task.
