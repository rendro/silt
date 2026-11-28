---
title: "channel / task"
section: "Standard Library"
order: 13
---

# channel

Bounded channels for concurrent task communication. Channels provide
communication between the tasks that the calls of `task` run.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `close` | `(Channel(a)) -> ()` | Close the channel |
| `each` | `(Channel(a), Fn(a) -> ()) -> ()` | Call a function with each message until the channel closes |
| `fold_until` | `(Channel(a), b, Fn(b, a) -> Step(b)) -> b` | Fold the messages until a step gives `Stop` or the channel closes; leaves the channel open |
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
  task.join(
    [
      { ->
        channel.send(ch, 1)
        channel.send(ch, 2)
        channel.close(ch)
      },
      { -> channel.each(ch) { msg -> println(msg) } },
    ],
  )
  -- prints 1, then 2
}
```


## `channel.fold_until`

```
channel.fold_until(ch: Channel(a), init: b, f: Fn(b, a) -> Step(b)) -> b
```

Receives messages from the channel and folds them into a state, starting
from `init`: `f` is given the state and each message, and gives
`Continue(next)` to go on with `next` or `Stop(value)` to end. The call
gives that value, or the last state when the channel is closed and empty.
It is how an actor keeps its state: every arm of the `match` says what
comes next, so an arm that forgets to go on does not type-check.

A `Stop` leaves the channel as it is: open, with the messages after the
one that stopped the fold still in it, for another reader. The fold asks
for one message at a time, so a stream read with it is not read ahead (to
stop a stream at `Stop`, use `stream.fold_until`). A stream that fails
makes the fold raise the failure, as `channel.each` does.

In the example the message type has a `Stop` of its own, which hides
the prelude's in its module: there the step is written `Step.Stop(count)`.

```silt
import channel
import task

type Msg {
  Add(Int),
  Get(Channel(Int)),
  Stop,
}

fn counter(inbox: Channel(Msg)) -> Int {
  channel.fold_until(inbox, 0) { count, msg ->
    match msg {
      Add(n) -> Continue(count + n)
      Get(reply) -> {
        channel.send(reply, count)
        Continue(count)
      }
      Stop -> Step.Stop(count)
    }
  }
}

fn main() {
  let inbox = channel.new(10)
  let reply = channel.new(1)
  let total = channel.new(1)
  task.join(
    [
      { -> channel.send(total, counter(inbox)) },
      { ->
        channel.send(inbox, Add(2))
        channel.send(inbox, Add(3))
        channel.send(inbox, Get(reply))
        println(channel.receive(reply)) -- Some(5)
        channel.send(inbox, Stop)
      },
    ],
  )
  println(channel.receive(total)) -- Some(5)
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

The capacity is a limit, not memory taken up front: a channel holds only what
is in it. The largest capacity is 10,000,000.

```silt
import channel

fn main() {
  let _rendezvous = channel.new(0) -- true rendezvous (capacity 0)
  let _buffered = channel.new(10) -- buffered (capacity 10)
  let _large = channel.new(10_000_000) -- as small as the other two while empty
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
-- no per-call OS thread. A stop of the task that waits (see
[Stops](#stops)) takes it off the channel and out of the timer.

```silt
import channel
import task
import time

fn main() {
  let ch = channel.new(0)
  let sender = { ->
    time.sleep(time.ms(50))
    channel.send(ch, 42)
  }
  task.background([sender]) { ->
    match channel.receive_timeout(ch, time.ms(500)) {
      channel.Message(v) -> println(v) -- 42
      channel.Empty -> println("timed out")
      channel.Closed -> println("channel closed")
    }
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
  let boss = { ->
    channel.send(jobs, 1)
    channel.send(jobs, 2)
    channel.close(quit)
  }
  let done = task.background([boss]) { -> work(jobs, quit, 0) }
  println(done) -- 3
}
```

Receive-only form:

```silt
import channel

fn main() {
  let ch1 = channel.new(1)
  let ch2 = channel.new(1)
  channel.send(ch2, "hello")
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

Run work at the same time, and catch its failures. Each call that starts
tasks returns only when they have all ended, and a failure of one of them
is raised in the caller. Tasks are multiplexed onto a fixed thread pool,
run in parallel, and communicate through channels. See
[Concurrency](../concurrency.md) for how the calls fit a program.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `background` | `(List(Fn() -> ()), Fn() -> a) -> a` | Run helper functions while the body runs, stop them when it returns, and give its value |
| `deadline` | `(Duration, Fn() -> a) -> Option(a)` | Run a function, and end it when the duration has passed: Some of its value, or None |
| `each` | `(Channel(a), Int, Fn(a) -> ()) -> ()` | Call a function on each value of a channel, at most at_once calls at a time, until the channel is closed and empty |
| `first_ok` | `(List(Fn() -> Result(a, e))) -> Result(a, List(e))` | Run functions at the same time, give the first Ok and stop the others, or every Err in order |
| `guard` | `(Fn() -> a) -> Result(a, RuntimeError)` | Run a function, and give a runtime error it raises as an `Err` |
| `join` | `(List(Fn() -> ())) -> ()` | Run functions at the same time, and return when all of them have returned |
| `map` | `(List(a), Int, Fn(a) -> b) -> List(b)` | Call a function on each item of a list, at most at_once calls at a time, and give the values in the list's order |


## `task.join`

```
task.join(pieces: List(Fn() -> ())) -> ()
```

Runs each function of `pieces` as a task of its own, all at the same
time, and returns when every one of them has returned. What the pieces
did (a send, a file they wrote) is done when the call returns. An empty
list returns at once.

If a piece fails (a runtime error: a `panic`, a division by zero), the
other pieces are stopped and `task.join` raises that failure in the
caller. Its report names the place in the piece where it happened, and
below the piece's own calls the calls that led to the `task.join`; a
`task.guard` around the join catches it like any runtime error. A piece
that fails while the others are being stopped is a note under the first
failure (`another piece failed too, at main.silt:7:3: panic: ...`).

A piece is stopped as every task is (see
[Stops](#stops)): it takes nothing new, a wait it is in
ends, and a piece that only computes ends within about 2,000 steps.
`task.join` returns, or raises, only when every piece has ended: no piece
runs on after it.

A stop from outside reaches every piece: when the time of a
`task.deadline` around the join is over, the pieces end with its
function; a stop of the task that waits in the join stops them,
and so does an enclosing `task.join` or `task.background` that stops.

When no piece can go on and the program's own thread waits in the join,
the program is deadlocked: the report says `task.join with no progress
possible`, with a line for each piece that waits and what it waits on,
placed where the piece's function is written (``a piece of `task.join`
at main.silt:7:5 waits to receive from <channel:0>``).

```silt
import channel
import list
import task

fn sum(ch: Channel(Int), acc: Int) -> Int {
  match channel.receive(ch) {
    Some(n) -> sum(ch, acc + n)
    None -> acc
  }
}

fn main() {
  let results = channel.new(3)
  task.join(list.map([1, 2, 3]) { n -> { -> channel.send(results, n * 10) } })
  channel.close(results)
  println(sum(results, 0)) -- 60
}
```

A failure of a piece, caught around the join:

```silt
import task

fn check(n: Int) -> () {
  match n > 2 {
    true -> panic("{n} is too big")
    false -> ()
  }
}

fn main() {
  match task.guard({ -> task.join([{ -> check(1) }, { -> check(3) }]) }) {
    Ok(_) -> println("all pieces passed")
    Err(e) -> println(e.message) -- panic: 3 is too big
  }
}
```

## `task.background`

```
task.background(helpers: List(Fn() -> ()), body: Fn() -> a) -> a
```

Runs each helper as a task while `body` runs (as a task too), and gives
the value of `body`. When `body` returns, the helpers that still run are
stopped, and the call returns when they have ended. Helpers are for work
that serves the body and has no end of its own: a worker that answers
requests on a channel, a ticker, a logger.

A helper is stopped (see [Stops](#stops)): it takes nothing new, not
even what is already in its channel. Work that must be finished (a
channel drained to its end) belongs in a `task.join` with the code that
closes the channel when it is done.

If a helper or the body fails, the others are stopped and the failure is
raised in the caller, as in `task.join`; a failure during that stop is a
note under it. A helper that fails while it is being stopped, after the
body has returned, fails the call too: the caller gets the failure, not
the body's value. A stop from outside reaches the body and every helper, and
a deadlock reads `task.background with no progress possible`, with a
line for the body and each helper that waits. With no helpers,
`task.background([], body)` is `body()`.

```silt
import channel
import task

fn square_server(requests: Channel(Int), answers: Channel(Int)) -> () {
  channel.each(requests) { n -> channel.send(answers, n * n) }
}

fn ask(requests: Channel(Int), answers: Channel(Int), n: Int) -> Int {
  channel.send(requests, n)
  match channel.receive(answers) {
    Some(answer) -> answer
    None -> 0
  }
}

fn main() {
  let requests = channel.new(0)
  let answers = channel.new(0)
  let total = task.background([{ -> square_server(requests, answers) }]) { ->
    ask(requests, answers, 3) + ask(requests, answers, 4)
  }
  println(total) -- 25
}
```

## `task.each`

```
task.each(work: Channel(a), at_once: Int, f: Fn(a) -> ()) -> ()
```

Runs `at_once` workers, each a task, that take values from `work` and
call `f` with each one, until the channel is closed and empty; then
`task.each` returns. Every value is taken by one worker, once, and at
most `at_once` calls of `f` run at a time. The channel is not closed by
the call: whoever sends on it closes it when the work is all sent (a
stream, such as one of `stream.from_fn`, closes when its feed returns).
A stream that failed fails the worker that reads its end, as a receive
would; when several workers read it, that is one failure, reported once.

`at_once` must be 1 or more: `task.each(ch, 0, f)` is a runtime error
(`task.each: at_once is 0, and must be 1 or more`). An `at_once` above
the limit of 100,000 tasks that can run at once is refused the same way,
before any worker starts.

The workers are the pieces of a [`task.join`](#taskjoin) in every other
way: a call of `f` that fails stops the other workers and its failure is
raised in the caller; a stop from outside (the time of a `task.deadline`
around the call, a stop of the task that waits in it, an enclosing
`task.join` or `task.background` that stops) stops every worker, and
nothing of the call runs on after it. A stopped worker takes no further
value. When no worker can go on and the program's own thread waits in
the call, the deadlock report reads `task.each with no progress
possible`, with a line for each worker that waits (``a worker of
`task.each` at main.silt:9:3 waits to receive from <channel:0>``).

```silt
import channel
import list
import stream
import string
import task

fn main() {
  let pages = stream.from_fn { ch -> list.each(1..6) { i -> channel.send(ch, "page {i}") } }
  let sizes = channel.new(6)
  task.each(pages, 3) { page -> channel.send(sizes, string.length(page)) }
  channel.close(sizes)
  let total = list.fold(list.map(1..6) { _ -> channel.receive(sizes) }, 0) { acc, size ->
    match size {
      Some(n) -> acc + n
      None -> acc
    }
  }
  println(total) -- 36
}
```

## `task.map`

```
task.map(xs: List(a), at_once: Int, f: Fn(a) -> b) -> List(b)
```

Calls `f` with each item of `xs`, at most `at_once` calls at a time
(each worker a task that takes the next item when its call has
returned), and gives the values in the order of `xs`, whichever call
returned first. `task.map([], n, f)` is `[]`. `at_once` must be 1 or
more, as for `task.each`; with more than there are items, one worker
runs for each item.

A call of `f` that fails stops the other workers and is raised in the
caller, as in `task.join`, and a stop from outside stops every worker:
a stopped worker starts no further call.

```silt
import task

fn main() {
  let squares = task.map([1, 2, 3, 4, 5], 2) { n -> n * n }
  println(squares) -- [1, 4, 9, 16, 25]
}
```

## `task.first_ok`

```
task.first_ok(works: List(Fn() -> Result(a, e))) -> Result(a, List(e))
```

Runs each function of `works` as a task, all at the same time. The first
that gives `Ok(value)` wins: the others are stopped, and the call gives
`Ok(value)` when they have ended. The winner is not stopped: a stream it
built and gave in its `Ok` comes out whole. If every function gives an `Err`, the
call gives `Err` of the list of their errors, in the order of `works`
(not the order they ended in); `task.first_ok([])` is `Err([])`.

A runtime error in a function is no `Err`: it stops the others and is
raised in the caller, as in `task.join`, also when it comes while the
others are stopped after an `Ok`. A stop from outside stops every
function, and a deadlock reads `task.first_ok with no progress possible`.

```silt
import task

fn lookup(replica: String, key: String) -> Result(Int, String) {
  match replica {
    "b" -> Ok(42)
    _ -> Err("{replica}: no {key}")
  }
}

fn main() {
  let found = task.first_ok([{ -> lookup("a", "x") }, { -> lookup("b", "x") }])
  println(found) -- Ok(42)
  let missing = task.first_ok([{ -> lookup("a", "y") }, { -> lookup("c", "y") }])
  println(missing) -- Err(["a: no y", "c: no y"])
}
```

## Stops

A task that a call of this module started is stopped when the call no
longer needs it: the body of [`task.background`](#taskbackground)
returned (its helpers), a function of [`task.first_ok`](#taskfirst_ok)
gave `Ok` (the others), another task of the call failed, the time of a
[`task.deadline`](#taskdeadline) passed (its function), nobody reads a
`stream.from_fn` stream any more (its `feed`), or the task that waits in
the call was stopped itself. A stop reaches every task that the stopped
task started, in calls inside it however deep, and the stages of the
streams it built. The call that stops tasks returns when they have
ended, and every task below them: their last steps (below) have run.
What comes after it (closing the channel the tasks sent on, counting
what they sent, removing a file they wrote) sees everything they did.

A stop is no value in the stopped work and no runtime error: the work
ends where it is, nothing after that place runs, and a `task.guard` in
it does not catch the stop. What a stopped task returns in its last steps
is dropped. A runtime error in them is not lost: it is a note under the
failure that caused the stop, or, where nothing failed (the time of a
deadline passed, a body returned, a function gave `Ok`), the failure of
the call.

Its effect on the task depends on where the task is at that moment:

- **The task is waiting** (parked in a channel receive or send, a
  `channel.select`, a call that waits for its own tasks, a `time.sleep`,
  I/O, or similar): its wait ends without taking or sending anything,
  and the task is ended there. A value sent afterwards goes to the next
  receiver, not to it.
- **The task has never run** (started, and not yet picked up by a
  worker): it is dropped without running.
- **The task is not waiting**: it is running, or its time slice ran out
  and it is to run again, or **its wait has just ended** (a sender
  handed it a value, a receiver took the value it waited to send, the
  operation it waited for gave its result) and it has not run again. The
  task goes on with what it holds, for a bounded amount of further
  work. **After a stop a task takes nothing new; it may still give
  what it holds.** It is ended:
  - at its next operation that would take something, whether or not
    the thing is there: a `channel.receive`, `channel.try_receive` or
    `channel.receive_timeout`, the next message of a `channel.each`,
    the next element of a stream, `tcp.accept` (the connection stays
    with the listener), and a `channel.select` of which no `Send` arm can
    be done at once (its `Recv` arms, timeouts among them, are not
    tried);
  - at its next operation that would have to wait, with nothing sent;
    and at one that would start something which waits for the outside:
    reading or writing a file's contents, the network, a database,
    standard input. Nothing is started: a file it was about to write is
    not written;
  - at a call that would start a task: `task.join`, `task.background`,
    `task.each`, `task.map`, `task.first_ok`, `task.deadline`, and
    every stream function that starts a stage (`stream.map`,
    `stream.take` and the other transforms, and the sources:
    `stream.from_list`, `stream.from_fn`, `stream.tcp_lines`, ...). No
    task is started, so nothing is taken from the channel a stream would
    have been built over;
  - when it has run 2,000 steps (about 2,000 instructions, a few
    hundred rounds of a small loop), if it reaches none of these
    before. The steps are counted from the first time slice that begins
    after the stop, so a task that was computing when the stop came
    is ended at most one time slice and 2,000 steps later. (A step that
    is one long call, such as a sort of a long list or a function of the
    host, runs to its end first.)

  What is done in those steps is what gives: a `channel.send` into a
  channel with room or to a receiver that waits, `channel.try_send`, a
  `Send` arm of a `channel.select` that can be done at once,
  and `channel.close`. A `time.sleep` of no time is done, since it waits
  for nothing; any longer one is a wait. (The `fs` functions, which never
  wait, are done too: `fs.exists`, `fs.remove`, `fs.rename` and the
  others.) The side effects of those steps happen.

  This is the rule of a function that is past its `task.deadline`, and
  the same count: a task that is past its deadline and stopped by its
  call as well has 2,000 steps in all, not twice that.

**What the further work is for, and what it does not give.** A task that
was just given something has the steps to pass it on by an operation
that does not wait: a worker that receives an item and puts it, or its
result, into a channel with room does not lose the item to a stop, and
takes no further one. A stop is still no orderly shutdown. A worker that
receives an item, computes, and then has to *wait* to send its result
(its output channel is full, or is a rendezvous channel whose receiver
is not there yet) is ended in that send, and the item is gone; so is the
item of a worker that computes on it for more than 2,000 steps, or that
has to read or write a file for it. Where nothing may be lost when the
work stops, tell it to finish over a channel (the close of its input, a
stop message), and let a `task.join` wait for it.

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
a call that waits for its own tasks, a wait deep inside the callback of a
`list.fold`, or the read of a stream. A `channel.receive` that nobody will
ever answer is no deadlock inside a deadline: it ends when the deadline
has passed. It is the rule of a task that is stopped (see [Stops](#stops),
which lists what takes and what gives).

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
  `channel.close` are done. That is what a function that got its value
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
  after any stop.
- **A failure of the function is the caller's**: a runtime error inside
  `work` is raised by `task.deadline` as it is, at the place in `work` where it
  happened, whatever the clock says. A function that fails is never
  reported as timed out.
- **I/O that was given up may have happened.** An I/O operation in flight
  when the function is ended is given up as for a stopped task (see
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
- **A deadline ends its function and what it started.** The tasks of the
  calls inside the function (pieces, helpers, workers) and the stages of
  the streams it built are stopped with it, however deep (see
  [Stops](#stops)), and `task.deadline` answers when they have ended.
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

To bound each item of a [`task.map`](#taskmap) by itself, put the
deadline inside `f`: `task.map(xs, n) { x -> task.deadline(limit) { -> work(x) } }`
gives an `Option` for each item, in the order of `xs`.

## `task.guard`

```
task.guard(work: Fn() -> a) -> Result(a, RuntimeError)
```

Runs `work` and gives `Ok` of what it returns, or `Err(e)` if a runtime
error was raised inside it: a `panic`, a division by zero, a failed
assertion of a builtin, raised in `work` or in anything it calls (a
callback of `list.map`, the function of a `task.deadline`, a piece of a
`task.join` or of another call that starts tasks, a stream stage it
reads). The
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
- a stop of the task that runs the guard (see [Stops](#stops)): the end
  of a `task.deadline`'s function at its time, a helper whose body
  returned, a piece of a call in which another piece failed. The guard
  does not return, and nothing after it runs;
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
