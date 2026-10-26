---
title: "process"
section: "Standard Library"
order: 25
---

# process

The running process. `process.exit` ends a program with an exit code of its
own choosing. A program that ends any other way exits with 0, or with 1 when
it failed: a runtime error, a `main` that returns `Err`, or a task that failed
and that nobody joined.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `exit` | `(Int) -> Never` | End the program with an exit code |


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
        io.eprintln("count: {Error.message(e)}")
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
