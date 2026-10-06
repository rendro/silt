---
title: "stream"
section: "Standard Library"
order: 18
---

# stream

A library of channel-backed sources, transforms, and sinks. Streams are
simply [`Channel`](channel-task.md) values used as data flows — the
underlying primitive is unchanged. Each source and each transform is a
*stage*: a task that reads its input channel, calls the user closure, and
writes to the output channel. A stage waits like any task, so a pipeline
of many stages holds no thread while it waits. Backpressure is provided by
channel capacity (default 16; configurable via `stream.buffered`).

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

### Transforms

| Function | Signature |
|----------|-----------|
| `map` | `(Channel(a), Fn(a) -> b) -> Channel(b)` |
| `map_ok` | `(Channel(Result(a, b)), Fn(a) -> c) -> Channel(Result(c, b))` |
| `filter` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `filter_ok` | `(Channel(Result(a, b)), Fn(a) -> Bool) -> Channel(Result(a, b))` |
| `flat_map` | `(Channel(a), Fn(a) -> List(b)) -> Channel(b)` |
| `take` | `(Channel(a), Int) -> Channel(a)` |
| `drop` | `(Channel(a), Int) -> Channel(a)` |
| `take_while` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `drop_while` | `(Channel(a), Fn(a) -> Bool) -> Channel(a)` |
| `chunks` | `(Channel(a), Int) -> Channel(List(a))` |
| `scan` | `(Channel(a), b, Fn(b, a) -> b) -> Channel(b)` |
| `dedup` | `(Channel(a)) -> Channel(a) where a: Equal` |
| `buffered` | `(Channel(a), Int) -> Channel(a)` |

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
| `each` | `(Channel(a), Fn(a) -> ()) -> ()` |
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
    |> stream.filter({ n -> n % 2 == 1 })
    |> stream.map({ n -> n * n })
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
  let xs = stream.collect(stream.unfold(1, { n ->
    match n > 5 {
      true -> None
      false -> Some((n, n + 1))
    }
  }))
  println(xs)
}
```

## Design notes

- **Streams are channels.** No new value type. `stream.collect(ch)` works
  on any `Channel`, not just streams produced by this module.
- **Backpressure is automatic.** When the output channel of a stage is
  full, the stage waits for room, and so consumes no further input.
- **A failing stage fails the pipeline.** If the function of a stage
  raises a runtime error (a division by zero, a `panic`), the stage closes
  its output *with that failure*. The next stage fails with it in turn,
  and the sink at the end raises it: `stream.collect` does not return the
  values that came before the failure. `channel.each` on a stream raises
  it too; a plain `channel.receive` sees `Closed`.
- **A pipeline that is cut short stops.** When a stage or a sink reads no
  further (`take`, `take_while`, `first`, a failure downstream), the stage
  that feeds it is stopped, and so on up to the source. So
  `stream.repeat(x) |> stream.take(3)` leaves nothing running. The output
  of a stage is closed by this: read it with one consumer. A channel that
  no stage feeds (one you made with `channel.new`) is left open.
- **A stream that you stop reading goes on.** A stream that is read by
  hand (`channel.receive`, `channel.select`) and then left is still at
  work: its stages run until their outputs are full or the source is at
  its end, and the program does not end before they have stopped (see
  [When a program ends](../concurrency.md#when-a-program-ends)).
  `channel.close(s)` on the stream stops them at once.
- **Errors flow through the stream.** File sources emit
  `Channel(Result(_, IoError))`; TCP sources emit
  `Channel(Result(_, TcpError))`. Each chunk can fail independently;
  consumers pattern-match. Use `map_ok` / `filter_ok` to apply
  transformations only to `Ok` values, passing `Err(_)` through unchanged.
- **No async/await.** Stages are tasks of the silt scheduler; file and
  socket reads run on the I/O pool while the stage waits.
- **`stream.repeat` is infinite.** Always pair it with `take`,
  `take_while`, or another bounded sink — `collect` on an unbounded
  stream will hang.

## Forward compatibility

Function names mirror what method-form dispatch (`s.map(f)`) would look
like once silt grows a `Stream` trait. Existing silt programs will
continue to compile and behave identically when that trait lands.
