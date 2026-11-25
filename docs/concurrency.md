---
title: "Concurrency"
section: "Guide"
order: 3
description: "silt's concurrency: work that lives inside a call, typed channels, select, streams."
---

# Concurrency

Silt runs work at the same time in tasks. A task is light: a stack and
frames, not an OS thread. Tasks run in parallel on a fixed pool of
threads and talk to each other over channels. Every value in silt is
immutable, so a value sent over a channel can never be changed under
the task that receives it: there are no data races.

Everything concurrent lives in three modules: `task` starts work and
catches failures, `channel` carries values between tasks, and `stream`
builds pipelines. There are no concurrency keywords.


## 1. The Idea

**Concurrent work lives inside a call.** A call such as
`task.join(pieces)` starts a task for each piece and returns only when
every one of them has ended. Nothing it started runs on after it, and
there are no task handles to keep, join or forget. (A stream is the one
value with work behind it: its stages run while it is read, and are
stopped when nobody reads it any more.)

**A failure inside the call is a failure of that call.** When a piece
fails with a runtime error (a `panic`, a division by zero, a failed
assertion), the call stops the other pieces and raises the failure in
the caller, at the place where it happened. The program fails as it
would if the code had run in one task.

**Catching is a separate step.** `task.guard(work)` runs a function and
gives `Ok` of its value, or `Err(e)` for a runtime error raised in it.
It starts no task. A failure that a program expects (a file that is
missing, a bad line of input) is a `Result` value, as everywhere in
silt; `task.guard` is for the failures it does not expect, such as a
bug in one job of a long-running worker.

**A stop is not a value.** A call stops the work it started when that
work is no longer needed: `task.background` stops its helpers when its
body returns, `task.first_ok` stops the rest when one gives `Ok`, every
call stops the rest when one piece fails, and `task.deadline` stops its
work when the time is up. The work that is stopped just ends. Whoever
stopped it learns that from the call it made: `task.deadline` gives
`None`.

| Call | What it does |
|---|---|
| `task.join(pieces)` | Runs the functions as tasks, and returns when all have returned |
| `task.background(helpers, body)` | Runs helpers beside the body, stops them when the body returns, and gives its value |
| `task.map(xs, at_once, f)` | Calls `f` on each item, at most `at_once` at a time, and gives the values in order |
| `task.each(ch, at_once, f)` | Calls `f` on each value of a channel, at most `at_once` at a time, until it is closed and empty |
| `task.first_ok(works)` | Runs the functions together, gives the first `Ok` and stops the rest, or every `Err` in order |
| `task.deadline(limit, work)` | Runs `work`, and stops it when `limit` has passed: `Some` of its value, or `None` |
| `task.guard(work)` | Runs `work` in place, and gives a runtime error raised in it as an `Err` |
| `stream.from_fn(feed)` | Runs `feed` as the source of a stream, which closes when `feed` returns |

The reference for each is in [channel / task](stdlib/channel-task.md)
and [stream](stdlib/stream.md#streamfrom_fn).

### Why this shape

- **The code says how long the work lives.** A task lives as long as
  the call that started it, so the nesting of the calls is the lifetime
  of the tasks. Where the call ends, the work has ended.
- **No failure is lost.** A failure always has a caller to go to. It
  does not wait in a task that nobody looks at.
- **Code stays sequential.** Each task reads like straight-line code.
  The concurrency is in how the calls and the channels are put together,
  not in how each piece is written.
- **No colored functions.** There is no `async`. A `channel.receive`, an
  `io.read_file` or an `http.get` is an ordinary call that parks the
  task while it waits, and lets the thread run other tasks.

The tasks share what they capture: a closure that a call runs as a task
holds the values it uses, by reference, which is safe because no value
can change.


## 2. Channels

A channel is a conduit for passing values between tasks. This section shows
how the operations fit together; what each function returns and when it
fails is in the reference, [channel / task](stdlib/channel-task.md).

### Creating channels

```silt
-- Rendezvous channel (capacity 0 -- sender blocks until receiver is ready)
let ch = channel.new(0)

-- Buffered channel with capacity 10
let ch = channel.new(10)
```

`channel.new(n)` takes the channel's capacity: 0 makes a rendezvous channel,
more makes a buffer of that many values. Channels carry any value type,
even other channels; the type is inferred from usage.
([`channel.new`](stdlib/channel-task.md#channelnew))

### Sending and receiving

```silt
channel.send(ch, "hello")

when let Some(msg) = channel.receive(ch) else {
  return
}
```

A send into a full channel, and a receive from an empty one, park the task
until the other side comes; the OS thread runs other tasks meanwhile.
`channel.receive` gives `Some(value)`, or `None` once the channel is closed
and its buffer is empty. Before it receives, the scheduler gives competing
receivers their turn, so values fan out round-robin.
([`channel.send`](stdlib/channel-task.md#channelsend),
[`channel.receive`](stdlib/channel-task.md#channelreceive))

### Closing: `channel.close(ch)`

```silt
channel.close(ch)
```

Closing says that no more values will come. Receivers still get what is in
the buffer, then `None`. A send on a closed channel is a runtime error, so a
channel is closed by the side that sends on it, or after the senders are
done. ([`channel.close`](stdlib/channel-task.md#channelclose))

### Without waiting: `channel.try_send` and `channel.try_receive`

```silt
let ch = channel.new(2)
println(channel.try_send(ch, "a")) -- true
println(channel.try_send(ch, "b")) -- true
println(channel.try_send(ch, "c")) -- false (buffer full)

println(channel.try_receive(ch)) -- Message("a")
```

The `try_` forms never park: `try_send` says whether the value went in, and
`try_receive` gives `Message(value)`, `Empty` (nothing now) or `Closed`
(nothing ever again). ([`channel.try_send`](stdlib/channel-task.md#channeltry_send),
[`channel.try_receive`](stdlib/channel-task.md#channeltry_receive))

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

`channel.each` is the channel's `list.each`: it calls the function for each
value and returns when the channel is closed and empty. After each value it
gives the other tasks that wait on the channel their turn, which is what
spreads work over the workers of `task.each`.
([`channel.each`](stdlib/channel-task.md#channeleach))

### Unbuffered channels (true rendezvous)

`channel.new(0)` creates a true rendezvous channel, with capacity 0. The
sender blocks until a receiver is ready, and the receiver
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

task.join(
  [
    { -> channel.send(ch, "hello") },
    { ->
      -- this receive and the send of the other piece meet
      when let Some(msg) = channel.receive(ch) else {
        return
      }
      println(msg) -- hello
    },
  ],
)
```

### Waiting with a time limit

`channel.timeout(dur)` makes a channel that closes after `dur`; a `Recv` arm
on it in a `channel.select` puts a time limit on the select.
`channel.receive_timeout(ch, dur)` is that race for one channel in one call:
it gives `Message(value)`, `Empty` when the time ran out, or `Closed`, and a
value that is already there wins over the timer.

```silt
let ch = channel.new(10)
let timer = channel.timeout(time.ms(5000)) -- closes after 5 seconds

match channel.select([channel.Recv(ch), channel.Recv(timer)]) {
  channel.Received(^ch, Some(val)) -> println("got: {val}")
  channel.Received(^timer, None) -> println("timed out after 5s")
  _ -> ()
}

match channel.receive_timeout(ch, time.ms(500)) {
  channel.Message(val) -> println("got: {val}")
  channel.Empty -> println("timed out")
  channel.Closed -> println("channel closed")
}
```

([`channel.timeout`](stdlib/channel-task.md#channeltimeout),
[`channel.receive_timeout`](stdlib/channel-task.md#channelreceive_timeout))


## 3. Running Work Together

### `task.join(pieces)`

`task.join` runs each function of the list as a task, all at the same
time, and returns `()` when all of them have returned. A piece gives
nothing back: what it finds, it sends on a channel, or the work is a
`task.map`, which gives the values.
([`task.join`](stdlib/channel-task.md#taskjoin))

```silt
import channel
import task

fn main() {
  let ch = channel.new(10)
  task.join(
    [
      { ->
        channel.send(ch, "hello")
        channel.send(ch, "from")
        channel.send(ch, "silt")
        channel.close(ch)
      },
      { -> channel.each(ch) { msg -> println(msg) } },
    ],
  )
}
-- prints:
--   hello
--   from
--   silt
```

The first piece produces, the second consumes. The producer closes the
channel when it is done, which ends the consumer's `channel.each`, and
`task.join` returns when both have returned.

### `task.background(helpers, body)`

`task.background` runs the helpers as tasks while `body` runs, and gives
the value of `body`. When the body returns, the helpers that still run
are stopped, and the call returns when they have ended. A helper is for
work that serves the body and has no end of its own: a worker that
answers requests on a channel, a ticker, a server.
([`task.background`](stdlib/channel-task.md#taskbackground))

```silt
import channel
import task
import time

fn main() {
  let ticks = channel.new(8)
  let ticker = { ->
    loop {
      time.sleep(time.ms(10))
      let _ = channel.try_send(ticks, ())
      loop()
    }
  }
  let seen = task.background([ticker]) { ->
    let _ = channel.receive(ticks)
    let _ = channel.receive(ticks)
    2
  }
  println("{seen} ticks") -- 2 ticks
}
```

The ticker would tick for ever; it ends because the body returned. A
stopped helper takes nothing new, not even what is already in its
channel: work that must be finished belongs in a `task.join`.

### `task.map(xs, at_once, f)`

`task.map` calls `f` with each item of the list, at most `at_once` calls
at a time, and gives the values in the order of the list, whichever
call ended first. It is the fan-out and fan-in in one call: the work is
spread over `at_once` workers, and the values come back as a list.
([`task.map`](stdlib/channel-task.md#taskmap))

```silt
import task
import time

fn slow_square(n: Int) -> Int {
  time.sleep(time.ms(10 * (5 - n)))
  n * n
}

fn main() {
  -- The last items end first; the list keeps their order all the same.
  let squares = task.map([1, 2, 3, 4], 4) { n -> slow_square(n) }
  println(squares) -- [1, 4, 9, 16]
}
```

`at_once` is how much runs at the same time: the number of files read at
once, of requests in flight to one server. It must be 1 or more.

### `task.each(ch, at_once, f)`

`task.each` runs `at_once` workers that take values from a channel and
call `f` with each, until the channel is closed and empty; then it
returns. Each value is taken by one worker, once. It is a worker pool:
the channel is the queue of jobs.
([`task.each`](stdlib/channel-task.md#taskeach))

```silt
import channel
import list
import stream
import task

fn main() {
  let jobs = channel.new(10)
  let results = channel.new(10)
  list.each([10, 20, 30, 40, 50, 60]) { n -> channel.send(jobs, n) }
  channel.close(jobs)

  -- Three workers share the jobs.
  task.each(jobs, 3) { n -> channel.send(results, n * 2) }
  channel.close(results)

  let total = stream.fold(results, 0) { sum, n -> sum + n }
  println(total) -- 420
}
```

The channel is not closed by the call: whoever sends the jobs closes it
when they are all sent. Workers that wait on one channel take turns:
every worker gets work and none starves. The split is fair, not
ordered: which worker takes which job depends on the timing of the run.

### `task.first_ok(works)`

`task.first_ok` runs the functions together. The first that gives
`Ok(value)` wins: the others are stopped, and the call gives
`Ok(value)`. If every one gives an `Err`, the call gives `Err` of the
list of their errors, in the order of the list.
([`task.first_ok`](stdlib/channel-task.md#taskfirst_ok))

```silt
import task
import time

fn fetch(mirror: String, ms: Int) -> Result(String, String) {
  time.sleep(time.ms(ms))
  match mirror {
    "b" -> Err("b is down")
    _ -> Ok("the file, from {mirror}")
  }
}

fn main() {
  -- Ask three mirrors; the first that has the file wins.
  let mirrors = [{ -> fetch("a", 300) }, { -> fetch("b", 10) }, { -> fetch("c", 50) }]
  println(task.first_ok(mirrors)) -- Ok("the file, from c")
  let down = [{ -> fetch("b", 10) }, { -> fetch("b", 20) }]
  println(task.first_ok(down)) -- Err(["b is down", "b is down"])
}
```

Mirror `b` answers first, with an `Err`, which does not win; `c` gives
the first `Ok`, and `a` is stopped in its sleep.

### Calls inside calls

The calls nest. A piece of a `task.join` may run a `task.map` of its
own, a helper may run a `task.each`, a `task.deadline` may hold a whole
`task.background`. A stop or a failure goes through them all: when a
call stops a piece, every task the piece started, in calls inside it
however deep, is stopped too, and the call returns when all of them have
ended.

## 4. Stops

### What stops a task

A task that a call started is stopped when the call no longer needs it:

- the body of `task.background` returned: its helpers that still run;
- a function of `task.first_ok` gave `Ok`: the others;
- a task of the call failed: the other tasks of the call;
- the time of a `task.deadline` passed: its work, and everything the work
  started;
- nobody reads a `stream.from_fn` stream any more: its `feed`;
- the task that waits in the call is stopped itself: every task of the
  call.

A stop is no value in the work that is stopped, and no error: the work
ends, `task.guard` does not catch it, and nothing after the place where
it ended runs. The call that stopped it goes on.

### What a stop does

What a stop does to a task depends on where the task is.

- **A task that waits** (in a receive, a send, a `channel.select`, a
  sleep, I/O, a call that waits for its own tasks) is ended there,
  without taking or sending anything. A value sent afterwards goes to
  the next receiver.
- **A task that has not run yet** never runs.
- **A task that is running, or whose wait has just ended,** goes on for a
  bounded amount of further work: **after a stop a task takes nothing
  new; it may still give what it holds.** A worker that has just received
  an item can still put its result into a channel with room. It is ended
  at its next operation that would take something, wait, or start I/O,
  or after 2,000 steps.

A stopped call returns when every task it stopped has ended, so what
comes after it sees everything they did: closing the channel they sent
on loses nothing they sent. A stop is no orderly shutdown. Where
nothing may be lost, tell the work to finish over a channel (the close
of its input, a stop message), and let a `task.join` wait for it. The
whole rule, step by step, is in the reference:
[Stops](stdlib/channel-task.md#stops).


## 5. Failures

### A failure is the caller's

A runtime error in a piece of a call stops the other pieces and is
raised in the caller. The report shows the place where the error
happened, and below the piece's own calls the calls that led to
`task.join`:

```silt
-- noexec
import task

fn check(n: Int) -> () {
  match n > 2 {
    true -> panic("{n} is too big")
    false -> ()
  }
}

fn main() {
  task.join([{ -> check(1) }, { -> check(3) }])
  println("after")
}
```

```text
error[runtime]: panic: 3 is too big
 --> main.silt:5:13
   |
 5 |     true -> panic("{n} is too big")
   |             ^^^^^^^^^^^^^^^^^^^^^^^ panic: 3 is too big

call stack:
  -> check  at main.silt:5:13
  -> closure in main  at main.silt:11:36
  -> main  at main.silt:11:3
```

`after` is never printed: `task.join` raised the failure, as a call that
fails does. The same holds for `task.background`, `task.map`,
`task.each`, `task.first_ok` and `task.deadline`. The first failure is the one that is raised; a failure
of another piece that comes while the pieces are being stopped is added
to its report as a note.

### Catching: `task.guard(work)`

`task.guard` runs `work` in place, on the task that calls it, and gives
`Ok` of what it returns, or `Err(e)` for a runtime error raised inside
it: in `work` itself, in what it calls, and in the tasks of a call inside
it (a piece of a `task.join`, the work of a `task.deadline`, a stage of a
stream it reads). The program goes on after the guard.
([`task.guard`](stdlib/channel-task.md#taskguard))

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

`e` is a `task.RuntimeError`, a record of the error's `message`, its
`location` (`file:line:col`) and its `trace`, the calls that led there.
`println("{e}")` shows the whole report, as the program would have
printed it.

What a guard does not catch, because it is no runtime error of the
program: `process.exit`, a stop of the task that runs the guard (the
guard does not return), a deadlock of the program's own thread, the step
budget of an embedder, and a fault of silt or its host.

Use a guard where the program can do something better than fail: a
worker that records a job that broke and goes on with the next, a
supervisor that starts a worker again. Around one job, not around the
whole program: a failure that nothing handles should end the program,
with its report.

### Supervision and restart

A supervisor is a loop around `task.guard`. Here each worker takes jobs
from a shared channel; a job with a bug ends the worker's run, and its
supervisor starts it again, at most three times:

```silt
import channel
import list
import task

fn do_job(id: Int, job: String) -> () {
  when job != "poison" else {
    panic("worker {id} cannot do {job}")
  }
  println("worker {id} handled {job}")
}

fn supervise(id: Int, jobs: Channel(String), restarts: Int) -> () {
  match task.guard({ -> channel.each(jobs) { job -> do_job(id, job) } }) {
    -- The channel is closed and empty: the work is done.
    Ok(_) -> ()
    Err(e) -> {
      println("{e.message}; starting worker {id} again")
      when restarts > 0 else {
        return
      }
      supervise(id, jobs, restarts - 1)
    }
  }
}

fn main() {
  let jobs = channel.new(10)
  list.each(["a", "poison", "b"]) { job -> channel.send(jobs, job) }
  channel.close(jobs)
  task.join([{ -> supervise(1, jobs, 3) }, { -> supervise(2, jobs, 3) }])
}
```

- **The supervisor is ordinary code.** There is no built-in supervision
  tree. The policy (how often to start again, when to give up, what to
  log) is a few lines of silt, and a recursion is the loop.
- **A job that fails costs only its own run.** The guard is around the
  worker's loop, so the job that broke is gone, and the jobs still in the
  channel go to the next run.
- **What is not caught still ends the program.** A failure in
  `supervise` itself, outside the guard, is a failure of the
  `task.join`.

### Workers that fail

A worker can fail in three ways, and each has its own answer:

- **An expected failure is a value.** A job whose work can fail returns
  a `Result`, and the worker matches on it.
- **A bug is a runtime error.** `task.guard` around the job turns it
  into an `Err` that the worker can record.
- **A job that hangs is stopped at a time.** `task.deadline` around the
  job gives `None` when its time has passed.

```silt
import task
import time

fn job(n: Int) -> Result(Int, String) {
  match n {
    1 -> Ok(10)
    2 -> Err("no such file")
    3 -> Ok(100 / (n - 3))
    _ -> {
      time.sleep(time.hours(1))
      Ok(0)
    }
  }
}

fn run(n: Int) -> String {
  match task.guard({ -> task.deadline(time.ms(100)) { -> job(n) } }) {
    Ok(Some(Ok(value))) -> "job {n} gave {value}"
    Ok(Some(Err(reason))) -> "job {n} failed: {reason}"
    Ok(None) -> "job {n} took too long"
    Err(e) -> "job {n} has a bug: {e.message}"
  }
}

fn main() {
  println(run(1)) -- job 1 gave 10
  println(run(2)) -- job 2 failed: no such file
  println(run(3)) -- job 3 has a bug: division by zero
  println(run(4)) -- job 4 took too long
}
```

A server needs none of this for its handlers: `http.serve` answers
`500` to a request whose handler fails, writes the failure to stderr as
one line, and goes on serving
([`http.serve`](stdlib/http.md#connections-and-handlers)).


## 6. Time Limits

### Deadlines: `task.deadline(limit, work)`

```silt
let outcome = task.deadline(time.ms(200)) { -> io.read_file("/var/log/slow.log") }
```

`task.deadline(limit, work)` runs `work` and gives `Some` of what it
returns, or `None` if `work` was stopped because `limit` had passed. The
function runs as a task of its own, and when its time is over it is
stopped: it **takes nothing new, and may still give what it holds**, for
a bounded amount of further work. A wait it is in ends one way or the
other, never both: a receive under a deadline that gives `None` took no
value, so a loop that tries again loses nothing. Whatever `work` waits
for (I/O, a sleep, a channel, its own tasks, a stream), the deadline ends
it; a function that never waits, one that only computes, is ended within
about 2,000 steps; and what `work` started is stopped with it. A failure
inside `work` is raised by `task.deadline`, in the caller. The value of
`work` is inside the `Some`, so a function that returns a `Result` gives
two layers. The rule, step by step, is in the reference:
[`task.deadline`](stdlib/channel-task.md#taskdeadline).

```silt
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

A deadline bounds everything inside it. Around a `task.join`, it stops
every piece when the time is up; inside each piece of a `task.map`, it
bounds each item by itself, and an item that takes too long gives
`None` in its place of the list:

```silt
import task
import time

fn lookup(n: Int) -> Option(Int) {
  task.deadline(time.ms(200)) { ->
    -- the odd items answer at once, the even ones never
    match n % 2 {
      0 -> time.sleep(time.hours(1))
      _ -> ()
    }
    n * 10
  }
}

fn main() {
  println(task.map([1, 2, 3, 4], 4) { n -> lookup(n) }) -- [Some(10), None, Some(30), None]
}
```

`SILT_IO_TIMEOUT` is another thing: it bounds each single wait for I/O,
for the whole process, and gives the module's own timeout error
(see [I/O timeouts](#io-timeouts-silt_io_timeout)).

To wait for one value of a channel no longer than so long, use
`channel.receive_timeout` or a `channel.timeout` arm in a select (see
[Waiting with a time limit](#waiting-with-a-time-limit)).

## 7. Select

### `channel.select(ops)`

`channel.select` lets a task wait on several channel operations at once and
does the first that can be done: a `Recv(ch)` arm or a `Send(ch, value)`
arm. It gives `Received(channel, Some(value))`, `Received(channel, None)`
for a channel that is closed and drained, or `Sent(channel)`, naming the
channel of the arm that was done. ([`channel.select`](stdlib/channel-task.md#channelselect))

```silt
match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
  channel.Received(^ch1, Some(val)) -> handle_input(val)
  channel.Received(^ch2, Some(val)) -> handle_other(val)
  channel.Received(_, None) -> println("all done")
  _ -> ()
}
```

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
  channel.Received(^urgent, Some(msg)) -> println("URGENT: {msg}")
  channel.Received(^normal, Some(msg)) -> println("normal: {msg}")
  channel.Received(_, None) -> println("all closed")
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
  channel.Received(_, Some(val)) -> println("got {val} from somewhere")
  channel.Received(_, None) -> println("all done")
  _ -> ()
}
```

### Variable binding

You can also bind the channel to a new variable to inspect it later. A channel
has no `Display`, so log the payload (or a counter) instead of the
channel itself:

```silt
match channel.select([channel.Recv(ch1), channel.Recv(ch2)]) {
  channel.Received(_source, Some(val)) -> println("got {val}")
  channel.Received(_, None) -> println("all done")
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
`channel.Received(channel, None)`. A `Send` on a closed channel is no branch
at all: a
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


## 8. Streams

A stream is a channel whose values are made by a stage that the
runtime runs: a source, or a transform of another stream. The
[stream](stdlib/stream.md) module has the sources, the transforms
(`stream.map`, `stream.filter`, ...) and the sinks (`stream.collect`,
`stream.fold`, ...).

### A producer of your own: `stream.from_fn(feed)`

`stream.from_fn` runs `feed` as the source of a stream. What `feed`
sends on the channel it is given is the stream; the stream closes when
`feed` returns. A runtime error in `feed` reaches the reader, as a
failing stage's does, and when nobody reads the stream any more (it is
cut short, closed, or left), `feed` is stopped.
([`stream.from_fn`](stdlib/stream.md#streamfrom_fn))

```silt
import channel
import stream

fn main() {
  let raw = stream.from_fn { ch ->
    channel.send(ch, 1)
    channel.send(ch, 2)
    channel.send(ch, 3)
  }
  channel.each(stream.map(raw) { n -> n * 2 }) { n -> println("got: {n}") }
}
-- prints:
--   got: 2
--   got: 4
--   got: 6
```

A producer that would never end is no problem: its reader decides how
much it takes.

```silt
import channel
import stream

fn naturals(ch: Channel(Int), n: Int) -> () {
  channel.send(ch, n)
  naturals(ch, n + 1)
}

fn main() {
  let squares = stream.from_fn { ch -> naturals(ch, 1) } |> stream.map { n -> n * n }
  println(stream.take(squares, 4) |> stream.collect) -- [1, 4, 9, 16]
}
```

`stream.take` reads four values and cuts the stream short, which stops
the `map` stage and then `feed`.

### Pipelines

Each stage reads one channel and writes the next:

```
[source] ----> [map] ----> [filter] ----> [sink]
```

```silt
import stream

fn main() {
  let total = stream.from_list([1, 2, 3, 4, 5, 6])
    |> stream.map { n -> n * 10 }
    |> stream.filter { n -> n > 20 }
    |> stream.fold(0) { sum, n -> sum + n }
  println(total) -- 180
}
```

The stages run at the same time, each a task of the runtime, and work
ahead of their reader by a few values. A stage that fails fails the
pipeline: the sink at the end raises the failure. A pipeline that is cut
short stops every stage before the cut. A stage of your own that needs
more than a function of one value (state across values, several values
for one) is a `stream.from_fn` that reads the stream before it.

### Receiving from channels of different types

The operations of one `channel.select` are of one type, and so are the
channels of one `stream.merge`. To receive from channels that carry
different types, give them one: declare an enum with a variant for each,
turn each channel into a channel of the enum with `stream.map`, and
merge those. One `match` then handles every kind of message, and the
merged channel closes when all of its inputs have.

```silt
import channel
import stream

type Event {
  Key(String),
  Tick(Int),
}

fn main() {
  let keys = stream.from_fn { ch -> channel.send(ch, "q") }
  let ticks = stream.from_fn { ch ->
    channel.send(ch, 1)
    channel.send(ch, 2)
  }
  let from_keys = stream.map(keys) { k -> Key(k) }
  let from_ticks = stream.map(ticks) { n -> Tick(n) }
  let events = stream.merge([from_keys, from_ticks])
  let count = stream.fold(events, 0) { seen, event ->
    match event {
      Key(k) -> println("key {k}")
      Tick(n) -> println("tick {n}")
    }
    seen + 1
  }
  println("{count} events") -- 3 events
}
```

The order of messages from different channels is the order they arrive
in; the messages of one channel keep theirs.


## 9. Patterns

### Fan-out and fan-in

Spread work over several tasks and collect what they give: that is
`task.map`. The list of inputs is the fan-out, the list of values the
fan-in, in the order of the inputs.

```silt
import list
import string
import task

fn words(line: String) -> Int {
  list.length(string.split(line, " "))
}

fn main() {
  let lines = ["the quick brown fox", "jumps over", "the lazy dog"]
  let counts = task.map(lines, 2) { line -> words(line) }
  println(counts) -- [4, 2, 3]
  println(list.fold(counts, 0) { sum, n -> sum + n }) -- 9
}
```

### A worker pool

A queue of jobs that is filled while the workers run is a channel and
a `task.each`. The producer and the pool are two pieces of a
`task.join`; the producer closes the queue when the jobs are all sent,
and the pool returns when the queue is empty.

```
              +---> [worker 1] ---+
              |                   |
[jobs] -------+---> [worker 2] ---+------> [results]
              |                   |
              +---> [worker 3] ---+
```

```silt
import channel
import list
import stream
import task

fn main() {
  let jobs = channel.new(4)
  let results = channel.new(16)
  task.join(
    [
      { ->
        list.each(1..10) { n -> channel.send(jobs, n) }
        channel.close(jobs)
      },
      { -> task.each(jobs, 3) { n -> channel.send(results, n * n) } },
    ],
  )
  channel.close(results)
  println(stream.fold(results, 0) { sum, n -> sum + n }) -- 385
}
```

### A server with a helper

A server and the work beside it: the server is a helper of
`task.background`, and the body is what the program does while it
serves. When the body returns, the server is stopped.

```silt
import bytes
import http
import task
import tcp
import url

fn hello(_req: http.Request) -> http.Response {
  http.Response { status: 200, body: bytes.from_string("hi"), headers: #{} }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {
    panic("cannot listen")
  }
  let port = tcp.local_port(listener)
  when let Ok(home) = url.parse("http://127.0.0.1:{port}/") else {
    panic("no URL")
  }
  let status = task.background([{ -> http.serve(listener, hello) }]) { ->
    match http.get(home) {
      Ok(response) -> response.status
      Err(_) -> 0
    }
  }
  println(status) -- 200
}
```

A server that runs until it is told to stop is the other way round: the
server is the body, and the helpers are what serves it (a worker that
takes the jobs the handlers queue, a ticker that cleans a cache). The
body returns when the listener is closed (`tcp.close_listener`) and the
requests in flight are answered; then the helpers are stopped.

```silt
-- noexec
let queued = task.background([{ -> task.each(jobs, 4) { job -> run_job(job) } }]) { ->
  http.serve(listener, handler)
}
```

### Racing mirrors

Ask several sources for the same thing and take the first answer that
is good: that is `task.first_ok` (see
[`task.first_ok`](#taskfirst_okworks)). Put a `task.deadline` around it
to bound the wait for all of them together, or around each function to
bound each by itself.

### Graceful shutdown via `channel.close`

`channel.close` tells a stage that no more work comes. A worker that
reads with `channel.each` (or `task.each`) finishes what is in the
channel and returns; nothing is lost and no stop is needed.

```silt
import channel
import task

fn main() {
  let work = channel.new(10)
  task.join(
    [
      { ->
        channel.each(work) { item -> println("processing: {item}") }
        -- channel.each returned: the work channel is closed and empty
        println("shutdown complete")
      },
      { ->
        channel.send(work, "task-a")
        channel.send(work, "task-b")
        channel.send(work, "task-c")
        channel.close(work)
      },
    ],
  )
}
-- prints:
--   processing: task-a
--   processing: task-b
--   processing: task-c
--   shutdown complete
```

This needs no sentinel values or "poison pills": the closed channel is
the signal.

### Multiplexing with select

`channel.select` merges several inputs in one loop. When more than one
channel is ready, `select` picks a branch fairly at random, so the order
of the list gives no priority.

```silt
import channel
import task

fn main() {
  let alerts = channel.new(5)
  let logs = channel.new(5)
  task.join(
    [
      { ->
        channel.send(logs, "background task done")
        channel.send(logs, "log rotation complete")
        channel.close(logs)
      },
      { ->
        channel.send(alerts, "disk full!")
        channel.close(alerts)
      },
      { ->
        -- Merge both inputs into one handler, until one of them closes.
        loop {
          match channel.select([channel.Recv(alerts), channel.Recv(logs)]) {
            channel.Received(^alerts, Some(msg)) -> {
              println("alert: {msg}")
              loop()
            }
            channel.Received(^logs, Some(msg)) -> {
              println("log: {msg}")
              loop()
            }
            channel.Received(_, None) -> println("a channel closed")
            _ -> loop()
          }
        }
      },
    ],
  )
}
```

For priority ("always drain `alerts` before `logs`"), check the
high-priority channel first with `channel.try_receive`, and fall back to
`channel.select` only when it is empty.

### Draining with try_receive

To collect what is in a channel without knowing how much:

```silt
import channel

fn drain(ch) {
  match channel.try_receive(ch) {
    channel.Message(val) -> {
      println("got: {val}")
      drain(ch)
    }
    channel.Empty -> return println("no more data (channel still open)")
    channel.Closed -> return println("channel closed, all done")
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


## 10. The End of a Program

### When a program ends

A program ends when `main` returns. Every task that a call started has
ended by then, since each call returns only when its tasks have. A
stream that `main` still holds when it returns is stopped with its
stages.

- **A piece that never returns keeps its call, and the program, going.**
  A server in a piece of a `task.join` serves until its listener is
  closed. A loop that never ends belongs in a helper of
  `task.background`, which stops it when the body returns, or under a
  `task.deadline`.
- **A failure of `main` is the program's.** If `main` fails, with a
  runtime error or by returning `Err`, the program has failed and
  `silt run` exits with status 1.
- **`process.exit(code)` ends the program at once,** from `main` or from
  any task, with that exit code: every other task stops where it is.
  A program that must finish its work first lets its calls return, then
  calls `process.exit` (see [process](stdlib/process.md)).

Under `silt test`, each test is the same: a failure inside a call of the
test fails that test (see [Testing](language/testing.md)).

### Deadlocks

A program is deadlocked when `main` waits, every task waits too, and
nothing is pending that could end one of the waits: no timer and no I/O.
silt sees that at the moment it becomes true, and the wait of `main`
fails with a runtime error that starts with `deadlock on main thread`;
the program exits with status 1. When `main` waits in a call, the error
names the call and has a line for each of its tasks that waits:

```silt
-- noexec
import channel
import task

fn main() {
  let ch: Channel(Int) = channel.new(0)
  task.join([{ -> println(channel.receive(ch)) }, { -> println("other") }])
}
```

```text
other
error[runtime]: deadlock on main thread: task.join with no progress possible
 --> main.silt:6:3
   |
 6 |   task.join([{ -> println(channel.receive(ch)) }, { -> println("other") }])
   |   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ deadlock on main thread: task.join with no progress possible
  = help: a piece of `task.join` at main.silt:6:3 waits to receive from <channel:0>
```

A deadlock is no runtime error of the program: `task.guard` does not
catch it. The usual causes are a channel that nobody closes (a
`channel.each` that waits for ever) and a producer that ended before it
sent.

A wait that will end by itself counts as able to go on: a `time.sleep`, a
`channel.receive_timeout` that has not expired, a wait on a
`channel.timeout` channel that has not closed yet, the wait of a
`task.deadline` (a `channel.timeout` that no task waits on does not
count: it could wake nobody). While a task is in such a wait, no deadlock
is reported, whether or not that task could ever reach the channel that
`main` waits on. The verdict comes when the wait has ended, if the
program is still stuck then. So a task that sleeps for a minute delays
the report of an unrelated deadlock by up to a minute. The same holds
for a task that waits for an I/O operation.


## 11. Runtime Model

### Lightweight tasks on a thread pool

Tasks are lightweight -- each one is just a stack and frames, not an OS thread.
A call that starts tasks submits them to a fixed-size thread pool (sized to the
number of CPU cores). Many tasks are multiplexed onto a smaller number of OS threads, so
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

When a task performs a blocking operation (channel receive, select, a call that
waits for its tasks), it is parked -- the OS thread picks up another ready task instead of waiting. When the
blocking condition is satisfied (e.g., a value arrives on the channel), the
parked task is woken and rescheduled.

### Scheduler and time slicing

Silt uses a **cooperative** scheduler -- there is no wall-clock preemption.
A task holds its worker thread until one of two things happens:

1. It blocks on an I/O operation, channel send/receive/select, a call that
   waits for its tasks (`task.join` and the others), or `time.sleep`. The task is parked and the worker moves on to the next
   ready task.
2. It exhausts its **opcode time slice**. The scheduler runs each task for a
   fixed budget of bytecode instructions (default **2000 opcodes**); when the
   budget runs out, the task is re-queued at the back of the run queue and
   the worker picks up whoever is next. This keeps CPU-heavy tasks from
   starving each other.

The slice budget can be set with the `SILT_TIME_SLICE` environment variable,
which is read once, when the program starts: a whole number above 0. A
value silt cannot read (`abc`, `0`, `-5`) is refused: the program does
not start, and the error says why. A smaller value makes tasks take turns more often (for example,
`SILT_TIME_SLICE=500 silt run main.silt`). Its main use is testing and
debugging: with `SILT_TIME_SLICE=1` a task gives way after every
instruction, which brings out the faults that depend on where a task is
interrupted.

One consequence: a task running a tight CPU loop with no blocking calls will
still yield every ~2000 opcodes, but between those yield points it owns its
worker. If fewer long CPU-bound tasks run than there are workers, the
remaining workers stay idle; the scheduler does not migrate a running
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

- `io.*` surfaces `Err(Other("I/O timeout (SILT_IO_TIMEOUT exceeded)"))`. (The `fs`
  functions never wait for the I/O pool: they are not ended by it.)
- `tcp.*` surfaces `Err(Timeout)`.
- `http.*` surfaces `Err(Timeout)`.
- `postgres.*` surfaces `Err(Timeout)`: a connect, each statement, and
  in `postgres.transact` the wait for a connection with its `BEGIN`.
  The wait for its `COMMIT` or `ROLLBACK` has no such end, so that an
  `Err` never stands for a transaction that was committed.
- The `stream` functions that read or write a file or a connection
  (`stream.file_lines`, `stream.file_chunks`, `stream.write_to_file`,
  `stream.tcp_lines`, `stream.tcp_chunks`, `stream.write_to_tcp`) wait
  for I/O too: a source whose read times out ends with its module's error
  as its last item, a sink returns it.

The timeout is *invisible* at the language surface — no new syntax, no
colored functions — because every I/O builtin already returns `Result` with
the matching typed error enum. It is for waits for I/O only: a `time.sleep`
or a wait on a channel is not I/O and is not ended by it. (The connections
of `http.serve` have the server's own times instead, see
[http](stdlib/http.md).)

Accepted format: a whole number above 0 with a unit: `500ms`, `5s`, `2m`,
`1h`. Any other value (a bare number such as `5000`, `0s`, a typo) is
refused: `silt run`, `silt test` and `silt repl` do not start, and the
error names the variable, the value and the forms it takes. An empty value
is the same as none.

```sh
# Any I/O that stalls > 5s surfaces the module's typed timeout variant.
SILT_IO_TIMEOUT=5s silt run server.silt
```

To bound one piece of work rather than every wait of the process, use
[`task.deadline(limit, work)`](#deadlines-taskdeadlinelimit-work): it ends the
function it runs, whatever the function waits for, and says so with its own
`None`. The two are independent: inside a `task.deadline`, a wait
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
when it is stopped (see [What stops a task](#what-stops-a-task)), or when the program ends.
It resumes, or ends, at that moment, and from the moment of the stop
its operation is not wanted any more: an
operation that would take something it cannot put back (a client, a
connection of a database pool) and finds its task gone takes nothing.
What becomes of the operation is one rule for each kind of operation:

| Operation | What becomes of it when nobody waits for it |
|---|---|
| `tcp.connect`, `tcp.connect_tls`, `tcp.read`, `tcp.read_exact`, `tcp.write`, the TLS handshakes of `tcp.tls_server` and `tcp.tls_server_mtls`, and `stream.write_to_tcp` | The connection is **shut down**, as by `tcp.close`, which makes the operation return and frees its thread. A connect that waits for its peer (`tcp.connect`, `tcp.connect_tls`) is shut down too; on Linux that ends it at once, elsewhere it may run on to the system's connect time, and until the connect has returned it counts among the operations that cannot be interrupted (the bound of 64 below). The name lookup of a connect to a host name cannot be interrupted: it ends by the system's own time. |
| `http.get`, `http.request` | The request is **ended**: its connect, a read or a write it waits in, a TLS handshake, a redirect, which frees its thread. A connect that waits for its peer is shut down, as one of `tcp.connect` is: on Linux that ends it at once; elsewhere it may run on to its connect time (at most 10 s), counted until then among the operations that cannot be interrupted. The name lookup of a host name cannot be interrupted. |
| `stream.tcp_lines`, `stream.tcp_chunks` | The source **lets go** of the connection, which closes only if nothing else holds it. A read in flight ends without taking anything, on a plain or a TLS connection (see [stream](stdlib/stream.md)). |
| `tcp.accept`, and the accept of `http.serve` | The accept is woken and gives up; the listener stays usable. A client's connection that reached it is kept by the listener for the next accept. |
| `postgres.query`, `postgres.execute`, `postgres.notify` and the cursor functions, the `BEGIN` of `postgres.transact`, the `LISTEN` of `postgres.listen` | One that has no connection yet is **never sent**, and the connection goes back to the pool. One that has it: on a pool it is left to finish; in a transaction the server is asked to cancel it, and the transaction is aborted (see [postgres](stdlib/postgres.md)). |
| `postgres.connect`, `postgres.listen` that were done all the same | What they made is nobody's and is closed: the pool, the listener (its connection goes back to the pool). |
| `io.read_file`, `io.write_file`, `io.read_line`, `io.read_stdin`, `io.read_stdin_bytes`, `stream.file_*`, `stream.stdin_lines`, `stream.write_to_file` | It **cannot be interrupted**: it runs to its end, and its result is dropped. A write that had started completes, and a line that `io.read_line` reads afterwards is nobody's. |

In every case an operation that had not started yet never runs, and an
operation whose result arrives in the very moment its task stops waiting
counts as not taken: a connection is shut down all the same, a client
is kept, a pool is closed.

On a connection the rule means that the connection cannot be used after
a timeout: a read that timed out cannot be tried again. Give a read the
time the peer may really take, and treat a timeout as the end of the
connection.

An accept that waits costs nothing: its thread sleeps in the system
until a connection comes. To wake it the listener connects to its own
port on the loopback address; where that is not possible, say a packet
filter forbids it, the thread stays until a client connects.

An operation that cannot be interrupted (the files and standard input
of the last row, and on other systems than Linux a connect that waits for
its peer) no longer counts as one of the pool's while it runs
on, so operations that never return (a read of a terminal nobody types
at) do not use the pool up. Such stuck operations have a bound of their
own, 64: while that many are still running unheard, every new I/O
operation fails at once with an error that says so, instead of the
program taking a thread for each without end. Operations that were told
to stop (the rows above) do not count: they are on their way out. So
requests that a deadline gives up, also to hosts that do not answer the
connect (a link checker over hosts that are down), never use the bound on
Linux.

### Blocking operations

| Operation | Parks when |
|---|---|
| `channel.send(ch, val)` | Buffer is full -- resumes when space opens |
| `channel.receive(ch)` | Buffer is empty -- resumes when a value arrives or the channel closes |
| `task.join`, `task.background`, `task.each`, `task.map`, `task.first_ok`, `task.deadline` | Its tasks have not all ended -- resumes when they have |
| `channel.select([...ops])` | No `Recv`/`Send` op is ready -- resumes when any becomes ready |
| `time.sleep(duration)` | Always -- parks the task for the given duration, then resumes |
| `io.read_file(path)` | Always (file I/O) |
| `io.write_file(path, content)` | Always (file I/O) |
| `io.read_line()` | Always (stdin) |
| `io.read_stdin()`, `io.read_stdin_bytes()` | Always (stdin) |
| `http.get(u)` | Always (network) |
| `http.request(...)` | Always (network) |
| `print`, `println`, `io.eprint`, `io.eprintln` | The output's writer holds 64 KiB that its reader has not taken yet -- resumes when there is room |

None of these block the OS thread. The task is parked and the thread continues
running other tasks.

What a program prints to the process's output (the `silt` command's
`HostIo::process()`; an embedder's own outputs are written by the print
itself, see [the FFI](ffi.md#output-and-clock)) goes to one writer for stdout and stderr together,
which writes it out in the order of the calls, each text whole: two tasks'
lines never mix, and a line on stderr stays between the stdout lines it was
printed between (as `2>&1` shows them). A print does not wait for the reader. Only when the reader
has not taken 64 KiB of what was printed (a slow log shipper, a pager, a pipe
nobody reads yet) does a print wait for room, as any wait of a task: the other
tasks, their timers and deadlines go on, and a `task.deadline` around a print
that waits ends it (its text is not written). Everything printed is written out
before the program ends, and before `process.exit` ends it, as long as the
reader takes it. (A process ended by a signal, such as Ctrl-C, loses what the
writer still held: up to 64 KiB of prints that had returned.)

One order has a price: while stdout is stuck, the program's own
`io.eprintln` lines wait behind it too, and a task that logs to stderr waits
for room as one that prints to stdout does. A program that ends by a failure
(a runtime error, a panic, a deadlock) waits for its output only while the reader takes some of it, about a
second without progress at most; then the failure is reported on stderr, and
the program ends with its status, also where the reader of stdout has gone.
What was printed and not yet written then is dropped: a reader that pauses
longer than that second loses up to the last 64 KiB of the output.

I/O operations follow the same transparent yielding pattern as channel
operations -- no special syntax needed. When code calls `io.read_file` or
`http.get`, the operation is dispatched to an I/O pool and the task is
parked until the result is ready. `main` is a task like the others here:
its I/O and its `time.sleep` are waits too.

### Implications of real parallelism

- **True parallelism.** Multiple tasks execute simultaneously on different CPU
  cores. CPU-bound work gains from `task.map` and the other calls.
- **Lightweight tasks.** A task is a stack, not an OS
  thread. You can have tens of thousands of concurrent tasks.
- **Non-deterministic ordering.** The same inputs may produce different
  interleavings across runs.
- **No data races.** All silt values are immutable, so sending a value
  through a channel is always safe. There is no shared mutable state.
- **No colored functions.** Unlike async/await, there is no function coloring.
  `channel.send` and `channel.receive` look and act like normal function calls.
  I/O operations like `io.read_file` and `http.get` also transparently yield
  to the scheduler -- there is no `async io.read_file` or special I/O monad.


## 12. Limitations

- **No buffered channel resizing.** A channel's capacity is fixed at creation
  time.
- **A channel whose buffer holds the channel is kept until the program
  ends.** A value in a channel's buffer that holds that same channel (a
  record with the channel in a field, a closure that uses it) keeps the
  channel alive. When the program lets go of the channel while such a
  value is still buffered, the channel and everything in its buffer stay
  in memory until the program ends. Receive what is buffered before the
  last use of such a channel, or send a value that does not hold it.
- **At most 100,000 tasks at once.** A call that would start a task while
  100,000 tasks have not ended is a runtime error, "task limit exceeded:
  100000 tasks running (max 100000)". The tasks of every call count: the
  pieces and workers of the `task` calls, the work of `task.deadline`, the
  stages of the `stream` functions and the connections of `http.serve`.


## Quick Reference

Every function of `channel` and `task`, with its signature and what it
returns, is in the tables of the reference: [channel / task](stdlib/channel-task.md).

The mental model: a call that starts tasks is a block of concurrent work
that ends when its tasks have; channels are the pipes between the tasks,
`channel.select` is a multiplexer, and `task.guard` is where a failure
becomes a value. Tasks are lightweight and multiplexed onto a fixed thread
pool, giving you true parallelism without the cost of one OS thread per
task.
