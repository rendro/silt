---
title: "process"
section: "Standard Library"
order: 25
---

# process

The running process. `process.exit` ends a program with an exit code of its
own choosing; `process.stop_signals` lets it stop cleanly when it is asked to
stop (Ctrl-C, `kill`, a service manager). A program that ends any other way
exits with 0, or with 1 when it failed: a runtime error (also one in a task,
which the call that ran the task raises) or a `main` that returns `Err`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `exit` | `(Int) -> Never` | End the program with an exit code |
| `stop_signals` | `() -> Channel(Signal)` | A channel that the first SIGINT or SIGTERM is sent into, instead of ending the process |


## `process.exit`

```
process.exit(code: Int) -> Never
```

Ends the program at once with the exit code `code`, from `main` or from any
task. Every other task stops where it is, and nothing is waited for: no
running task, no sleep, no pending I/O. The failures of tasks that nobody
joined are not reported. What the program printed before the call is written
out to stdout and stderr; a file that a task streams to ends where that task
stopped. A program that must finish its work first tells its tasks to stop
over a channel and joins them, then calls `exit`.

An exit code is 0 to 255. Any other code is a runtime error, raised where
`exit` is called: `process.exit: exit code 256 is out of range 0 to 255`.
silt itself uses 1 (a failed program), 101 (an internal error) and 141
(stdout closed); a program may use these too.

Its type is `Never`, as `panic`'s is, so a call ends a match arm or an
`else` of any type:

```silt
import int
import io
import process

fn main() {
  match io.args() {
    [n] -> match int.parse(n) {
      Ok(n) -> println(n * 2)
      Err(e) -> {
        io.eprintln("count: {e}")
        process.exit(2)
      }
    }
    _ -> {
      io.eprintln("usage: count N")
      process.exit(2)
    }
  }
}
```

Under `silt test`, a call of `process.exit` fails the test it is in, whatever
the code, and the run goes on with the next test. To test a tool's exit code,
test the function that decides it (one that returns the code), and let `main`
pass it to `process.exit`.

In the REPL, `process.exit` ends the session with its code, as `:quit` does
with 0. When a task calls it, the entry that runs ends at once where it
waits, and within about a million steps where it computes; a `time.sleep` of
the entry itself ends first. While the prompt waits for a line, the session
ends when the next line is entered, and that line does not run. A program
that a Rust program runs through the embedding API ends its run with the
code, and the host's process goes on (see [Embedding](../ffi.md)).


## `process.stop_signals`

```
process.stop_signals() -> Channel(Signal)
```

Returns a channel that the next stop signal is sent into, instead of the
signal ending the process: SIGINT (Ctrl-C at a terminal) as
`process.Interrupt`, SIGTERM (`kill`, `systemctl stop`, `docker stop`) as
`process.Terminate`. Once it has come, the channel is closed, so a
`channel.receive` after it gives `None`. A program that never calls
`stop_signals` is ended by a stop signal at once, as the system ends it.

The signal after that one ends the process as it would have without the
call (a shell shows exit status 130 for SIGINT, 143 for SIGTERM): a shutdown
that is stuck is still stopped by a second Ctrl-C. A call after the signal
has come asks for the next one again. Each call returns a channel of its own,
and every channel asked for since the last stop signal gets it.

The program waits for the channel as for any other: `channel.receive`,
`channel.select` beside the channels of its work, `channel.try_receive`
between two jobs. A wait on it is never a deadlock, since a signal may come
at any time: a `main` that only waits for the stop runs until it is stopped,
and a task that still waits on it when `main` has returned keeps the program
running until the signal comes, as one that waits on a `channel.timeout` does
until its time. So a task that waits for the stop keeps a deadlock of the
other tasks from being reported: the program waits for the signal, and
when it comes reports the deadlock (exit 1). A run that must end by itself
gives the waits that can get stuck a `task.deadline`.
The program may send into the channel itself (`channel.send(stop,
process.Terminate)`), to stop from inside, or in a test.

A job runner that finishes the job it is running, saves its state and exits
with 0 when it is stopped:

```silt
-- noexec
import channel
import io
import process
import time

fn work(stop: Channel(process.Signal), done: Int) -> Int {
  time.sleep(time.ms(100)) -- stands for a job
  match channel.try_receive(stop) {
    channel.Empty -> work(stop, done + 1)
    _ -> done + 1
  }
}

fn main() {
  let stop = process.stop_signals()
  let done = work(stop, 0)
  let _ = io.write_file("jobs.txt", "{done}")
  println("stopped after {done} jobs")
}
```

A server stops the same way: the body of a `task.background` that runs
`http.serve` waits for the stop, and when it returns, the server stops.

```silt
import channel
import process

fn main() {
  let stop = process.stop_signals()
  channel.send(stop, process.Terminate)
  println(channel.receive(stop)) -- Some(Terminate)
}
```

A signal that the process ignored when the program called `stop_signals`
stays ignored: a command a shell script starts in the background (`cmd &`)
ignores SIGINT, and gets only SIGTERM. On Windows the stop signals are
Ctrl-C (`process.Interrupt`) and Ctrl-Break (`process.Terminate`) at the
console; closing the console window, a logoff and a shutdown end the process
as before. The playground has no signals: nothing is sent into the channel.

The signals are the process's: under `silt test` a test that calls
`stop_signals` takes them for the rest of the run, and in a program that a
Rust program runs through the embedding API, they are the host's process's
signals.


## `process.Signal`

```
type Signal {
  Interrupt, -- SIGINT: Ctrl-C (Ctrl-C on Windows)
  Terminate, -- SIGTERM: kill, a service manager (Ctrl-Break on Windows)
}
```

The stop signal a program was sent, as `process.stop_signals` gives it.
Shown, it is its name (`Interrupt`, `Terminate`).
