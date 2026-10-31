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
from its input only when its reader asks for one; a source works ahead of
its reader, up to 16 values, and `stream.buffered(n)` lets the stages
before it work up to `n` values ahead.

Sinks (`collect`, `fold`, `count`, etc.) drain a channel in the calling
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
| `tcp_chunks` | `(TcpStream, Int) -> Channel(Result(Bytes, TcpError))` | Read TCP in chunks |
| `tcp_lines` | `(TcpStream) -> Channel(Result(String, TcpError))` | Read TCP line-by-line |

`file_lines` reads the file up to 64 KiB of lines at a time and emits
them one by one, so it can be up to about 64 KiB ahead of what has been
received: a file that changes while it is read gives lines from before
the change as far as it is ahead. Under a step budget, a program reading
this way uses fewer steps, since the source waits once per read, not
once per line.

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
| `dedup` | `(Channel(a)) -> Channel(a) where a: Equal` |
| `buffered` | `(Channel(a), Int) -> Channel(a)` |

`buffered(ch, n)` passes every value on. It is the one transform that
takes values before they are asked for, up to `n` of them, so the stages
before it can run while its reader is still busy with an earlier value.
`buffered(ch, 0)` reads nothing ahead.

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
| `count` | `(Channel(a)) -> Int` |
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

## Design notes

- **Streams are channels.** No new value type. `stream.collect(ch)` works
  on any `Channel`, not just streams produced by this module.
- **A stage takes what its reader asks for.** A transform takes a value
  from its input only when its reader (a sink, the stage after it, or a
  read such as `channel.receive`) asks for one, and calls its function
  only on that value: `ch |> stream.map(f) |> stream.take(2)` takes two
  values from `ch` and runs `f` twice, and a stage whose output nobody
  reads takes nothing and computes nothing. A read that does not wait
  (`channel.try_receive`, a `channel.receive_timeout` that runs out) asks
  too: the value it asked for comes to a later read. A source does not
  wait to be asked: it works ahead of its reader until 16 values wait in
  its output (`file_lines` reads up to 64 KiB of lines at a time).
  `stream.buffered(n)` is the only transform that reads ahead, up to `n`
  values.
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
  is left in it stays there: a stage took only what was asked for, so `ch
  |> stream.map(f) |> stream.take(2)` takes exactly two values from `ch`,
  and a `filter` stops where its reader stops. Two things take more.
  `stream.buffered(n)` took up to `n` values ahead, which are gone when
  the pipeline is cut short. And a `merge`, a `zip` or a `channel.select`
  over several streams asks each of them, so each stage that feeds it may
  hold one value for a later read; when nothing reads it any more, that
  value is gone: at most one per input.
- **A TCP source or sink that is stopped closes its connection.** When
  `stream.tcp_lines` or `stream.tcp_chunks` is stopped before the peer has
  closed (`stream.tcp_lines(conn) |> stream.first`), or the task in
  `stream.write_to_tcp` is cancelled, the connection is shut down, as by
  `tcp.close`: a read that may be in flight has to end, and what the
  source had read ahead is gone anyway. That holds at whatever moment
  the source is stopped once it has begun to read: in a read, or with
  an item that nobody takes any more. (A source that is stopped before
  it has read anything leaves the connection as it is.) Read with
  `tcp.read` if the connection is to be used afterwards.
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
  consumers pattern-match. To change only the `Ok` values and pass each
  `Err(_)` through, map with `result.map`:
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
