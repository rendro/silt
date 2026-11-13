---
title: "stream"
section: "Standard Library"
order: 21
---

# stream

A library of channel-backed sources, transforms, and sinks. Streams are
simply [`Channel`](channel-task.md) values used as data flows — the
underlying primitive is unchanged. Each source and each transform is a
*stage*: a task that reads its input channel, calls the user closure, and
writes to the output channel. A stage waits like any task, so a pipeline
of many stages holds no thread while it waits. A transform takes a value
from its input only when its reader asks for one. A sink that reads to the
end asks for every value, so the stages before it work up to 16 values
ahead while it reads. A source works ahead of its reader, up to 16 values,
and `stream.buffered(n)` lets the stages before it work up to `n` values
ahead.

Sinks (`collect`, `fold`, `length`, etc.) drain a channel in the calling
task, which waits for each value.

See also [io / fs](io-fs.md) for the underlying file operations behind
`file_chunks` / `file_lines`, [tcp](tcp.md) for `tcp_chunks` / `tcp_lines`,
and [channel / task](channel-task.md) for the primitive channel operations.

## Summary

### Sources

| Function | Signature | Description |
|----------|-----------|-------------|
| `from_list` | `(List(a)) -> Channel(a)` | Emit list elements then close |
| `from_range` | `(Int, Int) -> Channel(Int)` | Emit `lo..=hi` then close |
| `repeat` | `(a) -> Channel(a)` | Infinite — pair with `take` |
| `unfold` | `(a, Fn(a) -> Option((b, a))) -> Channel(b)` | Generator (closes on `None`) |
| `file_chunks` | `(String, Int) -> Channel(Result(Bytes, IoError))` | Read file in chunks |
| `file_lines` | `(String) -> Channel(Result(String, IoError))` | Read file line-by-line |
| `stdin_lines` | `() -> Channel(Result(String, IoError))` | Read stdin line-by-line |
| `tcp_chunks` | `(TcpStream, Int) -> Channel(Result(Bytes, TcpError))` | Read TCP in chunks |
| `tcp_lines` | `(TcpStream) -> Channel(Result(String, TcpError))` | Read TCP line-by-line |

`file_lines` reads the file up to 64 KiB of lines at a time and emits
them one by one, so it can be up to about 64 KiB ahead of what has been
received: a file that changes while it is read gives lines from before
the change as far as it is ahead. `tcp_lines` likewise reads the
connection up to 64 KiB at a time and emits the lines that read
completed one by one; what came after the last of them waits for the
next read. Under a step budget, a program reading this way uses fewer
steps, since the source waits once per read, not once per line.

`stdin_lines` gives the lines of stdin, without their `\n` or `\r\n`, and
ends at the end of stdin; the text after the last `\n` is the last line. A
read takes what stdin has at that moment, up to 64 KiB, so a pipe gives many
lines a read and a line typed at a terminal comes as soon as it is typed. A
line that is not UTF-8 ends the stream with `Err(InvalidInput(...))` after
the lines before it, as for `file_lines`; its number counts the lines this
stream gave. A stream reads ahead of what it gives: once a program has made one,
the rest of stdin is its own, and an `io.read_line` or `io.read_stdin` after it
does not see what the stream has read but not given.

### Transforms

| Function | Signature |
|----------|-----------|
| `map` | `(Channel(a), Fn(a) -> b) -> Channel(b)` |
| `filter` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `flat_map` | `(Channel(a), Fn(a) -> List(b)) -> Channel(b)` |
| `take` | `(Channel(a), Int) -> Channel(a)` |
| `drop` | `(Channel(a), Int) -> Channel(a)` |
| `take_while` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `drop_while` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `chunks` | `(Channel(a), Int) -> Channel(List(a))` |
| `scan` | `(Channel(a), b, Fn(b, a) -> b) -> Channel(b)` |
| `enumerate` | `(Channel(a)) -> Channel((Int, a))` |
| `dedup` | `(Channel(a)) -> Channel(a) where a: Equal` |
| `buffered` | `(Channel(a), Int) -> Channel(a)` |

`buffered(ch, n)` passes every value on. It is the one transform that
takes values before they are asked for, whoever reads it, up to `n` of
them, so the stages before it can run while its reader is still busy with
an earlier value. `buffered(ch, 0)` reads nothing ahead.

`enumerate(ch)` pairs each value with its index, counting from 0 as
`list.enumerate` does: `(0, a)`, `(1, b)`, ...

### Combinators

| Function | Signature |
|----------|-----------|
| `merge` | `(List(Channel(a))) -> Channel(a)` |
| `concat` | `(List(Channel(a))) -> Channel(a)` |
| `zip` | `(Channel(a), Channel(b)) -> Channel((a, b))` |

### Sinks

| Function | Signature |
|----------|-----------|
| `collect` | `(Channel(a)) -> List(a)` |
| `fold` | `(Channel(a), b, Fn(b, a) -> b) -> b` |
| `length` | `(Channel(a)) -> Int` |
| `first` | `(Channel(a)) -> Option(a)` |
| `last` | `(Channel(a)) -> Option(a)` |
| `write_to_file` | `(Channel(Bytes), String) -> Result((), IoError)` |
| `write_to_tcp` | `(Channel(Bytes), TcpStream) -> Result((), TcpError)` |

## Examples

### Three-step pipeline

```silt
import stream

fn main() {
  let squares = stream.from_range(1, 100)
    |> stream.filter { n -> n % 2 == 1 }
    |> stream.map { n -> n * n }
    |> stream.take(5)
    |> stream.collect
  println(squares)
}
```

### The lines of stdin

```silt
import stream
import string

fn main() {
  -- Count the lines of stdin that are not empty.
  let filled = stream.stdin_lines()
    |> stream.filter { line ->
      match line {
        Ok(text) -> !string.is_empty(string.trim(text))
        Err(_) -> true
      }
    }
    |> stream.length
  println("{filled} lines with text")
}
```

### Generator via unfold

```silt
import stream

fn main() {
  -- Generate 1, 2, 3, 4, 5 then None.
  let xs = stream.collect(
    stream.unfold(1) { n ->
      match n > 5 {
        true -> None
        false -> Some((n, n + 1))
      }
    },
  )
  println(xs)
}
```

### Writing a stream of Result rows to a file

`stream.write_to_file` takes a channel of `Bytes`. A source that can fail
on the way, such as `postgres.stream` (a channel of `Result(Row,
PostgresError)`), is written by a relay task: it receives the rows, sends
the text of each `Ok` row on as bytes, and stops at the first `Err`,
which it returns; the file is written while the relay runs, so the rows
are never all held. Closing the source at the `Err` stops what feeds it.

```silt
import bytes
import channel
import io
import result
import stream
import string
import task

type ExportError {
  BadRow(String),
  Write(io.IoError),
}

-- Writes each Ok row to `path` as a line, as the rows come, and stops at
-- the first Err: Ok of the number of rows written, or the Err.
fn write_rows(rows: Channel(Result(String, String)), path: String) -> Result(Int, ExportError) {
  let lines = channel.new(64)
  let relay = task.spawn { ->
    let sent = loop n = 0 {
      match channel.receive(rows) {
        None -> Ok(n)
        Some(Err(e)) -> {
          channel.close(rows)
          Err(BadRow(e))
        }
        Some(Ok(row)) -> {
          channel.send(lines, bytes.from_string("{row}\n"))
          loop(n + 1)
        }
      }
    }
    channel.close(lines)
    sent
  }
  match stream.write_to_file(lines, path) {
    Ok(()) -> task.join(relay)
    Err(e) -> {
      task.cancel(relay)
      Err(Write(e))
    }
  }
}

fn main() {
  let good = stream.from_list([Ok("a,1"), Ok("b,2")])
  println(write_rows(good, "out.csv")) -- Ok(2)
  let bad = stream.from_list([Ok("a,1"), Err("row 2: no price"), Ok("c,3")])
  println(write_rows(bad, "out.csv")) -- Err(BadRow("row 2: no price"))
  -- The rows before the Err are in the file.
  println(io.read_file("out.csv") |> result.map(string.lines)) -- Ok(["a,1"])
}
```

## Design notes

- **Streams are channels.** No new value type. `stream.collect(ch)` works
  on any `Channel`, not just streams produced by this module.
- **A stage takes what its reader asks for.** A transform takes a value
  from its input only when its reader (a sink, the stage after it, or a
  read such as `channel.receive`) asks for one, and calls its function
  only on that value: `ch |> stream.map(f) |> stream.take(2)` takes two
  values from `ch` and runs `f` twice, and a stage whose output nobody
  reads takes nothing and computes nothing. A sink that reads to the end
  (`collect`, `fold`, `length`, `last`, `write_to_file`, `write_to_tcp`,
  `channel.each`) asks for every value: the stage before it works up to 16
  values ahead while it reads, and so does each stage before that, up to a
  `take`, a `take_while` or a `zip`, which ask for one value at a time, as
  `first` and a read by hand do. A read that does not wait
  (`channel.try_receive`, a `channel.receive_timeout` that runs out) asks
  too: the value it asked for comes to a later read, and is gone if the
  stream is closed or cut short before then (at most one value for each
  stage). A source does not wait to be asked: it works ahead of its
  reader until 16 values wait in its output (`file_lines` reads up to 64
  KiB of lines at a time). `stream.buffered(n)` is the only transform
  that reads ahead for any reader, up to `n` values.
- **A failing stage fails the pipeline.** If the function of a stage
  raises a runtime error (a division by zero, a `panic`), the stage closes
  its output *with that failure*. The next stage fails with it in turn,
  and the sink at the end raises it: `stream.collect` does not return the
  values that came before the failure. Every read of a stream raises it
  too: `channel.each`, `channel.receive`, `channel.try_receive`,
  `channel.receive_timeout` and a `Recv` arm of `channel.select`. A stage
  that took a value and failed on it, where nobody reads its output any
  more, is an error when the program ends, as a task that failed and was
  never joined is: "a `stream.map` stage failed and its output was never
  read". That is a stage that read ahead for `stream.buffered`, or one
  that a read which did not wait asked for a value. A failure that a read
  has raised is not reported again, a stage that nobody asked for a value
  computes nothing and so does not fail, and a pipeline that is cut short
  stays quiet.
- **A pipeline that is cut short stops.** When a stage or a sink reads no
  further (`take`, `take_while`, `first`, a failure downstream), the stage
  that feeds it is stopped, and so on up to the source. So
  `stream.repeat(x) |> stream.take(3)` leaves nothing running. The output
  of a stage is closed by this: read it with one consumer. A channel that
  no stage feeds (one you made with `channel.new`) is left open, and what
  is left in it stays there: a stage took only what was asked for, so
  `ch |> stream.map(f) |> stream.take(2)` takes exactly two values from
  `ch`, and a `filter` stops where its reader stops. Three things take
  more.
  `stream.buffered(n)` took up to `n` values ahead, which are gone when
  the pipeline is cut short. A sink that reads to the end but is stopped
  first (its task is cancelled, or its time under `task.deadline` runs
  out) loses up to 16 values for each stage before it. And a `merge`, a
  `zip` or a `channel.select` over several streams asks each of them, so
  each stage that feeds it may hold one value for a later read; when
  nothing reads it any more, that value is gone: at most one per input.
- **A TCP source or sink that is stopped closes its connection.** When
  `stream.tcp_lines` or `stream.tcp_chunks` is stopped before the peer has
  closed (`stream.tcp_lines(conn) |> stream.first`), or the task in
  `stream.write_to_tcp` is cancelled, the connection is shut down, as by
  `tcp.close`: a read that may be in flight has to end, and what the
  source had read ahead is gone anyway. That holds at whatever moment
  the source is stopped: in a read, with an item that nobody takes any
  more, or before it has read anything. Read with `tcp.read` if the
  connection is to be used afterwards.
- **A read or a write of a source or a sink waits for I/O**, and
  `SILT_IO_TIMEOUT` ends it as it ends any wait for I/O (see
  [I/O timeouts](../concurrency.md#io-timeouts-silt_io_timeout)): a
  source ends with its module's timeout error as its last item
  (`Err(Timeout)` of a connection, which is shut down then), a sink
  returns it. So does a read or a write that the I/O pool could not run:
  the source ends with that error, not as if its input had ended.
- **A stream that you stop reading stops taking.** A stream that is read
  by hand (`channel.receive`, `channel.select`) and then left takes
  nothing more from what feeds it: its transforms wait for a reader. Its
  source and a `stream.buffered` work on until their outputs are full or
  the source is at its end, and the program does not end before they have
  stopped (see [When a program
  ends](../concurrency.md#when-a-program-ends)). `channel.close(s)` on the
  stream stops them at once.
- **Errors flow through the stream.** File sources emit
  `Channel(Result(_, IoError))`; TCP sources emit
  `Channel(Result(_, TcpError))`. Each chunk can fail independently;
  consumers pattern-match. A file's error names its path, as `io`'s
  do (`file not found: app.log`). A line that is
  not UTF-8 is an error of `file_lines` and `tcp_lines` alike, and their
  last item: the source ends there, and `tcp_lines` shuts its connection
  down. `file_lines` names the line and the path
  (`InvalidInput("line 3 of app.log is not UTF-8")`). No byte is replaced:
  a program that wants the other lines of such a file reads it with
  `io.read_bytes` and decodes it with `bytes.to_string_lossy`, which
  writes U+FFFD for what is no UTF-8. To change only the `Ok` values and pass each `Err(_)`
  through, map with `result.map`:
  `ch |> stream.map { r -> result.map(r, f) }`.
- **No async/await.** Stages are tasks of the silt scheduler; file and
  socket reads run on the I/O pool while the stage waits.
- **`stream.repeat` is infinite.** Always pair it with `take`,
  `take_while`, or another bounded sink — `collect` on an unbounded
  stream will hang.
- **A channel is a Functor.** `Functor.map` of a `Channel(_)` is
  `stream.map` (see [Functors](../language/functors.md)), so generic code
  bounded by `Functor` starts a stage when it is given a channel. A
  channel has no `Applicative` or `Monad`.

A function over any Functor, given a stream:

```silt
import stream

fn double_all(xs: m(Int)) -> m(Int) where m: Functor {
  Functor.map(xs) { x -> x * 2 }
}

fn main() {
  let doubled = stream.from_list([1, 2, 3]) |> double_all |> stream.collect
  println(doubled) -- [2, 4, 6]
}
```


## `stream.from_list`

```
stream.from_list(xs: List(a)) -> Channel(a)
```

A stream of the elements of `xs`, closed after the last.

```silt
import stream

fn main() {
  println(stream.from_list(["a", "b"]) |> stream.collect) -- ["a", "b"]
}
```


## `stream.from_range`

```
stream.from_range(lo: Int, hi: Int) -> Channel(Int)
```

A stream of the numbers from `lo` to `hi`, both included, closed after the last.

```silt
import stream

fn main() {
  println(stream.from_range(1, 5) |> stream.collect) -- [1, 2, 3, 4, 5]
}
```


## `stream.repeat`

```
stream.repeat(x: a) -> Channel(a)
```

A stream of `x` without end: take a part of it.

```silt
import stream

fn main() {
  println(stream.repeat("hi") |> stream.take(3) |> stream.collect) -- ["hi", "hi", "hi"]
}
```


## `stream.unfold`

```
stream.unfold(seed: a, f: Fn(a) -> Option((b, a))) -> Channel(b)
```

A stream made from a seed: `f` gives `Some((value, next_seed))` for each value, and `None` to close it.

```silt
import stream

fn main() {
  let powers = stream.unfold(1) { n ->
    match n > 100 {
      true -> None
      false -> Some((n, n * 2))
    }
  }
  println(stream.collect(powers)) -- [1, 2, 4, 8, 16, 32, 64]
}
```


## `stream.file_chunks`

```
stream.file_chunks(path: String, size: Int) -> Channel(Result(Bytes, IoError))
```

The bytes of the file at `path`, in pieces of at most `size` bytes. A failure to open or read it is an `Err` element, and the stream ends there.

```silt
import bytes
import io
import stream

fn main() {
  let _ = io.write_file("data.bin", "0123456789")
  let sizes = stream.file_chunks("data.bin", 4)
    |> stream.map { chunk ->
      match chunk {
        Ok(b) -> bytes.length(b)
        Err(_) -> -1
      }
    }
  println(stream.collect(sizes)) -- [4, 4, 2]
}
```


## `stream.file_lines`

```
stream.file_lines(path: String) -> Channel(Result(String, IoError))
```

The lines of the file at `path`, without their line ends, as UTF-8 text. A failure to open or read it, or text that is no UTF-8, is an `Err` element, and the stream ends there.

```silt
import io
import stream

fn main() {
  let _ = io.write_file("notes.txt", "one\ntwo\nthree\n")
  println(stream.file_lines("notes.txt") |> stream.collect) -- [Ok("one"), Ok("two"), Ok("three")]
}
```


## `stream.stdin_lines`

```
stream.stdin_lines() -> Channel(Result(String, IoError))
```

The lines of stdin, without their line ends, until it ends. See [The lines of stdin](#the-lines-of-stdin).

```silt
import stream

fn main() {
  let n = stream.stdin_lines() |> stream.length
  println("{n} lines")
}
```


## `stream.tcp_chunks`

```
stream.tcp_chunks(stream: TcpStream, size: Int) -> Channel(Result(Bytes, TcpError))
```

The bytes that come on a TCP connection, in pieces of at most `size` bytes, until the other end closes it. A failure is an `Err` element, and the stream ends there.

```silt
import bytes
import stream
import tcp

fn main() -> Result((), tcp.TcpError) {
  let conn = tcp.connect("127.0.0.1:7000")?
  let total = stream.tcp_chunks(conn, 4096)
    |> stream.fold(0) { n, chunk ->
      match chunk {
        Ok(b) -> n + bytes.length(b)
        Err(_) -> n
      }
    }
  println("{total} bytes")
  Ok(())
}
```


## `stream.tcp_lines`

```
stream.tcp_lines(stream: TcpStream) -> Channel(Result(String, TcpError))
```

The lines that come on a TCP connection, without their line ends, until the other end closes it. A failure, or text that is no UTF-8, is an `Err` element, and the stream ends there.

```silt
import list
import stream
import tcp

fn main() -> Result((), tcp.TcpError) {
  let conn = tcp.connect("127.0.0.1:7000")?
  stream.tcp_lines(conn) |> stream.take(3) |> stream.collect |> list.each { line -> println(line) }
  Ok(())
}
```


## `stream.map`

```
stream.map(ch: Channel(a), f: Fn(a) -> b) -> Channel(b)
```

Each value of `ch` passed through `f`.

```silt
import stream

fn main() {
  println(stream.from_list([1, 2, 3]) |> stream.map { n -> n * 10 } |> stream.collect) -- [10, 20, 30]
}
```


## `stream.filter`

```
stream.filter(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
```

The values of `ch` for which `pred` is true.

```silt
import stream

fn main() {
  println(stream.from_range(1, 10) |> stream.filter { n -> n % 3 == 0 } |> stream.collect) -- [3, 6, 9]
}
```


## `stream.flat_map`

```
stream.flat_map(ch: Channel(a), f: Fn(a) -> List(b)) -> Channel(b)
```

The elements of the lists `f` gives for each value of `ch`, in order.

```silt
import stream

fn main() {
  println(stream.from_list([1, 2]) |> stream.flat_map { n -> [n, n] } |> stream.collect) -- [1, 1, 2, 2]
}
```


## `stream.take`

```
stream.take(ch: Channel(a), n: Int) -> Channel(a)
```

The first `n` values of `ch`. It stops reading `ch` after them.

```silt
import stream

fn main() {
  println(stream.from_range(1, 1000) |> stream.take(3) |> stream.collect) -- [1, 2, 3]
}
```


## `stream.drop`

```
stream.drop(ch: Channel(a), n: Int) -> Channel(a)
```

The values of `ch` after the first `n`.

```silt
import stream

fn main() {
  println(stream.from_range(1, 5) |> stream.drop(3) |> stream.collect) -- [4, 5]
}
```


## `stream.take_while`

```
stream.take_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
```

The values of `ch` up to the first for which `pred` is false, which is not passed on.

```silt
import stream

fn main() {
  println(stream.from_list([1, 2, 5, 1]) |> stream.take_while { n -> n < 3 } |> stream.collect) -- [1, 2]
}
```


## `stream.drop_while`

```
stream.drop_while(ch: Channel(a), pred: Fn(a) -> Bool) -> Channel(a)
```

The values of `ch` from the first for which `pred` is false on.

```silt
import stream

fn main() {
  println(stream.from_list([1, 2, 5, 1]) |> stream.drop_while { n -> n < 3 } |> stream.collect) -- [5, 1]
}
```


## `stream.chunks`

```
stream.chunks(ch: Channel(a), n: Int) -> Channel(List(a))
```

The values of `ch` in lists of `n`; the last list holds what is left.

```silt
import stream

fn main() {
  println(stream.from_range(1, 7) |> stream.chunks(3) |> stream.collect) -- [[1, 2, 3], [4, 5, 6], [7]]
}
```


## `stream.scan`

```
stream.scan(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> Channel(b)
```

Each step of a fold: `f(acc, value)` for every value, starting from `init`. The initial value itself is not passed on.

```silt
import stream

fn main() {
  println(stream.from_list([1, 2, 3, 4]) |> stream.scan(0) { acc, n -> acc + n } |> stream.collect) -- [1, 3, 6, 10]
}
```


## `stream.enumerate`

```
stream.enumerate(ch: Channel(a)) -> Channel((Int, a))
```

Each value of `ch` with its index, counting from 0.

```silt
import stream

fn main() {
  println(stream.from_list(["a", "b"]) |> stream.enumerate |> stream.collect) -- [(0, "a"), (1, "b")]
}
```


## `stream.dedup`

```
stream.dedup(ch: Channel(a)) -> Channel(a) where a: Equal
```

The values of `ch` without a value equal to the one before it.

```silt
import stream

fn main() {
  println(stream.from_list([1, 1, 2, 2, 1]) |> stream.dedup |> stream.collect) -- [1, 2, 1]
}
```


## `stream.buffered`

```
stream.buffered(ch: Channel(a), n: Int) -> Channel(a)
```

Every value of `ch`, read up to `n` values ahead of the reader, so the stages before it can work while the reader is busy with an earlier value.

```silt
import stream

fn main() {
  println(stream.from_range(1, 3) |> stream.buffered(16) |> stream.collect) -- [1, 2, 3]
}
```


## `stream.merge`

```
stream.merge(chs: List(Channel(a))) -> Channel(a)
```

The values of all of `chs`, in the order they come; it closes when all are closed.

```silt
import list
import stream

fn main() {
  let all = stream.merge([stream.from_list([1, 2]), stream.from_list([3])]) |> stream.collect
  println(list.sort(all)) -- [1, 2, 3]
}
```


## `stream.concat`

```
stream.concat(chs: List(Channel(a))) -> Channel(a)
```

The values of each stream of `chs` in turn: all of the first, then all of the second, and so on.

```silt
import stream

fn main() {
  println(stream.concat([stream.from_list([1, 2]), stream.from_list([3])]) |> stream.collect) -- [1, 2, 3]
}
```


## `stream.zip`

```
stream.zip(a: Channel(a), b: Channel(b)) -> Channel((a, b))
```

Pairs of a value of `a` and a value of `b`, in order; it closes when either closes.

```silt
import stream

fn main() {
  println(stream.zip(stream.from_list(["a", "b", "c"]), stream.from_range(1, 2)) |> stream.collect) -- [("a", 1), ("b", 2)]
}
```


## `stream.collect`

```
stream.collect(ch: Channel(a)) -> List(a)
```

Reads `ch` to its end and gives its values as a list.

```silt
import stream

fn main() {
  println(stream.from_range(1, 3) |> stream.collect) -- [1, 2, 3]
}
```


## `stream.fold`

```
stream.fold(ch: Channel(a), init: b, f: Fn(b, a) -> b) -> b
```

Reads `ch` to its end and gives `f(acc, value)` over its values, starting from `init`.

```silt
import stream

fn main() {
  println(stream.from_range(1, 10) |> stream.fold(0) { acc, n -> acc + n }) -- 55
}
```


## `stream.length`

```
stream.length(ch: Channel(a)) -> Int
```

Reads `ch` to its end and gives the number of values.

```silt
import stream

fn main() {
  println(stream.from_range(1, 10) |> stream.filter { n -> n % 2 == 0 } |> stream.length) -- 5
}
```


## `stream.first`

```
stream.first(ch: Channel(a)) -> Option(a)
```

The first value of `ch`, or `None` when it is closed without one. It reads no more of `ch`.

```silt
import stream

fn main() {
  println(stream.from_range(5, 9) |> stream.first) -- Some(5)
  println(stream.from_list([1]) |> stream.drop(1) |> stream.first) -- None
}
```


## `stream.last`

```
stream.last(ch: Channel(a)) -> Option(a)
```

Reads `ch` to its end and gives its last value, or `None` when it had none.

```silt
import stream

fn main() {
  println(stream.from_range(5, 9) |> stream.last) -- Some(9)
}
```


## `stream.write_to_file`

```
stream.write_to_file(ch: Channel(Bytes), path: String) -> Result((), IoError)
```

Writes the bytes of `ch` to the file at `path`, which it creates or empties, until `ch` closes. A failure to write is an `Err`.

```silt
import bytes
import io
import stream

fn main() {
  let lines = stream.from_list(["a", "b"]) |> stream.map { s -> bytes.from_string("{s}\n") }
  println(stream.write_to_file(lines, "out.txt")) -- Ok(())
  println(io.read_file("out.txt")) -- Ok("a\nb\n")
}
```


## `stream.write_to_tcp`

```
stream.write_to_tcp(ch: Channel(Bytes), stream: TcpStream) -> Result((), TcpError)
```

Writes the bytes of `ch` to a TCP connection until `ch` closes. A failure to write is an `Err`.

```silt
import bytes
import stream
import tcp

fn main() -> Result((), tcp.TcpError) {
  let conn = tcp.connect("127.0.0.1:7000")?
  let lines = stream.from_list(["one", "two"]) |> stream.map { s -> bytes.from_string("{s}\n") }
  stream.write_to_tcp(lines, conn)?
  tcp.close(conn)
  Ok(())
}
```
