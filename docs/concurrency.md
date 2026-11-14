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
spreads work over workers in Section 5.
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

let _ = task.spawn { ->
  -- this blocks until the main task calls channel.receive
  channel.send(ch, "hello")
}

-- this blocks until the spawned task calls channel.send
when let Some(msg) = channel.receive(ch) else {
  return
}
println(msg) -- hello
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


## 3. Tasks

### Spawning: `task.spawn(fn)`

`task.spawn` runs a function of no arguments as a lightweight task on the
thread pool and returns its `task.Handle` at once. Spawning is cheap: a
stack and frames, no OS thread. The function is a closure; since every value
is immutable, what it captures is shared with the task that spawned it
safely, by reference counting. ([`task.spawn`](stdlib/channel-task.md#taskspawn))

```silt
let multiplier = 10
let ch = channel.new(10)

let h = task.spawn { ->
  -- captures `multiplier` and `ch` from outer scope
  channel.send(ch, multiplier * 2)
}

task.join(h)
when let Some(val) = channel.receive(ch) else {
  return
} -- val = 20
```

### Joining: `task.join(handle)`

```silt
let h = task.spawn { -> 42 }
let result = task.join(h) -- result = 42
```

`task.join` parks until the task has ended and gives what its function
returned. A task that failed or was cancelled makes the join fail in turn,
with the runtime error `joined task failed: <msg>`: it is not an `Err`
value, and silt has no `try`/`catch`. Where a task may be cancelled as a
matter of course, let it signal its end over a channel instead.
([`task.join`](stdlib/channel-task.md#taskjoin))

### Cancelling: `task.cancel(handle)`

```silt
let results = channel.new(16)
let h = task.spawn { ->
  -- long-running work that sends what it finds
  channel.send(results, 42)
}
task.cancel(h)
-- The task has ended: closing its channel loses nothing it sent.
-- task.join(h) would raise `joined task failed: cancelled` here.
channel.close(results)
```

**A cancel reaches the tasks the task spawned.** `task.cancel(h)` also
cancels every task that `h`'s task spawned and that still runs, and every
one that those spawned, however deep: "stop this work" includes the work
it started. That holds also when `h`'s task has already returned, and
below a task between them that has returned: a worker that starts
children and returns is cancelled with its children. Each of them is
cancelled as `h` is, and `task.cancel` returns when all of them have
ended. A task that has already ended keeps its result: a failure of it
is still reported. A task that is spawned by a task that has been
cancelled is cancelled at once and never runs. The tasks the runtime
starts in a task's place count as spawned by it: the function of a
`task.deadline`, the stages of a stream, the connections of an
`http.serve`. A task meant to outlive the task that starts it is spawned
by one that is not cancelled: `main`, or a long-lived supervisor that
hands its children's work on.

```silt
import channel
import list
import task

fn main() {
  let cmds = channel.new(16)
  let worker = task.spawn { ->
    list.each([1, 2, 3]) { i ->
      let _ = task.spawn { -> channel.send(cmds, i) }
    }
  }
  task.cancel(worker)
  -- The worker's children are cancelled too: none of them sends on
  -- the closed channel, and nothing fails.
  channel.close(cmds)
}
```

What a cancel does to the task depends on where the task is. A task that
waits (in a receive, a send, a `channel.select`, a `task.join`, a sleep or
I/O) is ended there, without taking or sending anything; a task that has
not run yet never runs. A task that is running, or whose wait has just
ended, goes on for a bounded amount of further work: **after a cancel a
task takes nothing new; it may still give what it holds**, so a worker that
has just received an item can still put its result into a channel with
room. It is ended at its next operation that would take something, wait or
start I/O, or after 2,000 steps. A cancel is no orderly shutdown: where
nothing may be lost, tell the task to stop over a channel (a stop message,
or the close of its input), let it finish what it has, and join it.

`task.cancel` returns when the task, and every task below it that the
cancel reached, has ended, so what comes after it sees everything they did.
A `task.join` of a cancelled handle raises `joined task failed: cancelled`,
unless the task had already ended with another result, which the handle
keeps; a failure of a cancelled task is not reported. The whole rule, step
by step, is in the reference:
[`task.cancel`](stdlib/channel-task.md#taskcancel).

Returns `()`.

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
- A wait under a `task.deadline` is such a wait: it ends when the
  deadline has passed at the latest, so it is waited for. A task that
  nobody joined and that sits in
  `task.deadline(time.hours(1), { -> channel.receive(ch) })`, on a
  channel nobody will send on, keeps the program going for that hour
  after `main` has returned; without the deadline it would be dropped at
  once, as a wait that can never end.
- A task that waits for something only another task could give (a receive
  on a channel nobody will send on) is dropped when nothing else can run:
  such a wait could never end.
- A task that never stops keeps the program from ending. A ticker, a
  server loop or a watcher has to be told to stop, or cancelled, before
  `main` returns. That holds for a task that waits in I/O as well
  (`tcp.accept`, `io.read_line`): once it is cancelled nobody waits for
  the operation, which then counts for nothing. A server started with
  `http.serve` ends when the task that called it is cancelled, or when
  its listener is closed (`tcp.close_listener`), once it has answered the
  requests in flight.

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

A call of `process.exit(code)`, from `main` or from any task, ends the
program at once with that exit code: every other task stops where it is,
nothing is waited for, and the failures of tasks that nobody joined are not
reported. A program that must finish its work first tells its tasks to stop
over a channel and joins them, then calls `process.exit` (see
[process](stdlib/process.md)).

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
error[runtime]: a task failed and was never joined: division by zero
 --> main.silt:4:27
   |
 4 |   let _ = task.spawn { -> 1 / 0 }
   |                           ^^^^^ a task failed and was never joined: division by zero
  = help: the task was spawned at main.silt:4:11: join it with task.join to stop where you wait for it, or cancel it with task.cancel if you don't need its result
```

The report is rendered like every other runtime error: the file and line
where the task failed, the source line, and the call stack when there is
more than one frame. Its help names where the task was spawned, and says
so when the task that spawned it was cancelled. The rules:

- **A join handles the failure.** `task.join(h)` raises the task's error in
  the joiner (`joined task failed: <msg>`), and the failure is not reported
  a second time. A join in a task that has been cancelled takes nothing,
  so the failure stays unhandled.
- **A cancel handles the failure.** `task.cancel(h)` on a task that has
  failed dismisses the failure: no report, no exit status 1. A failure
  that `h` took with a join goes back to the task it joined, which is
  reported as never joined: the cancel of a task that joins never hides
  the failure of the task it joins, whenever the cancel comes. (Unless
  that task was cancelled too: then nothing is reported.)
- **Every failure counts.** The report is made when the program has ended
  (see [When a program ends](#when-a-program-ends)): a task that fails
  after `main` has returned still makes the run fail, every time. The
  example above does not have to wait for its task. (A stage of a stream
  is different: its failure goes to whoever reads the stream, and is
  reported only if no read raised it and the stream was not cut short. See
  [stream](stdlib/stream.md#design-notes).)
- **A deadlock shows its cause.** When `main` is told
  `deadlock on main thread`, the failures reported with it are usually the
  reason: a producer that failed before it sent.
- **An exit reports nothing.** After `process.exit` no failure is reported,
  and the exit code is the one the program gave: whether a task had failed
  by then is a race.

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
`Err(Timeout)` if `f` was ended because `dur` had passed. The function runs
as a task of its own, and when its time is over it is treated as a task
that was cancelled: it **takes nothing new, and may still give what it
holds**, for a bounded amount of further work. A wait it is in ends one way
or the other, never both: a receive that answers `Err(Timeout)` took no
value, so a loop that tries again loses nothing. Whatever `f` waits for
(I/O, a sleep, a channel, a join, a stream), the deadline ends it, and what
`f` spawned is cancelled with it. The value of `f` is inside the `Ok`, so a
function that returns a `Result` gives two layers. The rule, step by step,
is in the reference: [`task.deadline`](stdlib/channel-task.md#taskdeadline).

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
    Err(e) -> println("{e}")
  }
}
```

A task with a deadline is the two together:
`task.spawn { -> task.deadline(dur, f) }`, whose handle joins to the same
`Result`. That is the shape for fan-out where each child must bound its
own run time, such as racing replicas and dropping the stragglers.


## 4. Select

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
      channel.Received(^alerts, Some(msg)) -> {
        println("alert: {msg}")
        loop(())
      }
      channel.Received(^logs, Some(msg)) -> {
        println("log: {msg}")
        loop(())
      }
      channel.Received(_, None) -> {
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
import task

type Event {
  Key(String),
  Tick(Int),
}

fn main() {
  let keys = channel.new(4)
  let ticks = channel.new(4)
  let _ = task.spawn { ->
    channel.send(keys, "q")
    channel.close(keys)
  }
  let _ = task.spawn { ->
    channel.send(ticks, 1)
    channel.send(ticks, 2)
    channel.close(ticks)
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

### Graceful shutdown via channel.close

Use `channel.close` to signal that a pipeline stage is done. Downstream stages
detect closure via `channel.each` (which terminates) or by matching `None`
from `channel.receive`.

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
  when let Some(status) = channel.receive(done) else {
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

  when let Some(r1) = channel.receive(results) else {
    return
  }
  when let Some(r2) = channel.receive(results) else {
    return
  }
  when let Some(r3) = channel.receive(results) else {
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
  when let Some((id, outcome)) = channel.receive(outcomes) else {
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
- **A cancel of the supervisor stops its children.** `task.cancel` of
  the supervisor also cancels the workers it spawned that still run, and
  theirs: nothing is left behind.
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
`task.deadline` whose time is up, or when it is dropped at the end of
the program. It resumes, or ends, at that moment, and from the moment of
the cancel or the deadline its operation is not wanted any more: an
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
| `task.join(handle)` | Task not yet complete -- resumes when the task finishes |
| `channel.select([...ops])` | No `Recv`/`Send` op is ready -- resumes when any becomes ready |
| `time.sleep(duration)` | Always -- parks the task for the given duration, then resumes |
| `io.read_file(path)` | Always (file I/O) |
| `io.write_file(path, content)` | Always (file I/O) |
| `io.read_line()` | Always (stdin) |
| `io.read_stdin()`, `io.read_stdin_bytes()` | Always (stdin) |
| `http.get(url)` | Always (network) |
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
(a runtime error, a panic, a deadlock, a task that failed and that nobody
joined) waits for its output only while the reader takes some of it, about a
second without progress at most; then the failure is reported on stderr, and
the program ends with its status, also where the reader of stdout has gone.
What was printed and not yet written then is dropped: a reader that pauses
longer than that second loses up to the last 64 KiB of the output.

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
- **A channel whose buffer holds the channel is kept until the program
  ends.** A value in a channel's buffer that holds that same channel (a
  record with the channel in a field, a closure that uses it) keeps the
  channel alive. When the program lets go of the channel while such a
  value is still buffered, the channel and everything in its buffer stay
  in memory until the program ends. Receive what is buffered before the
  last use of such a channel, or send a value that does not hold it.
- **At most 100,000 tasks at once.** A `task.spawn` while 100,000 tasks
  have not ended is a runtime error, "task limit exceeded: 100000 tasks
  running (max 100000)". The tasks that the `stream` functions,
  `task.deadline` and `http.serve` start count too.


## Quick Reference

Every function of `channel` and `task`, with its signature and what it
returns, is in the tables of the reference: [channel / task](stdlib/channel-task.md).

The mental model: tasks are independent workers, channels are the pipes between
them, `channel.select` is a multiplexer, and `task.join` is a synchronization
barrier. Tasks are lightweight and multiplexed onto a fixed thread pool, giving
you true parallelism without the cost of one OS thread per task.
