---
title: "stream"
section: "Standard Library"
order: 18
---

# stream

A library of channel-backed sources, transforms, and sinks. Streams are
simply [`Channel`](channel-task.md) values used as data flows — the
underlying primitive is unchanged. Each transform spawns an internal pump
thread that reads its input channel, calls the user closure, and writes to
the output channel. Backpressure is provided by channel capacity (default
16; configurable via `stream.buffered`).

Sinks (`collect`, `fold`, `count`, etc.) drain a channel synchronously in
the calling task. Because every source and transform pump runs on a
dedicated OS thread (not a scheduler worker), sinks can safely block even
when called from a `task.spawn`'d task — producers keep making progress
regardless of scheduler state.

## Summary

### Sources

| Function | Signature | Description |
|----------|-----------|-------------|
| `from_list` | `(List(a)) -> Channel(a)` | Emit list elements then close |
| `from_range` | `(Int, Int) -> Channel(Int)` | Emit `lo..=hi` then close |
| `repeat` | `(a) -> Channel(a)` | Infinite — pair with `take` |
| `unfold` | `(a, (a) -> Option((b, a))) -> Channel(b)` | Generator (closes on `None`) |
| `file_chunks` | `(String, Int) -> Channel(Result(Bytes, String))` | Read file in chunks |
| `file_lines` | `(String) -> Channel(Result(String, String))` | Read file line-by-line |
| `tcp_chunks` | `(TcpStream, Int) -> Channel(Result(Bytes, String))` | Read TCP in chunks |
| `tcp_lines` | `(TcpStream) -> Channel(Result(String, String))` | Read TCP line-by-line |

### Transforms

| Function | Signature |
|----------|-----------|
| `map` | `(Channel(a), (a) -> b) -> Channel(b)` |
| `map_ok` | `(Channel(Result(a, e)), (a) -> b) -> Channel(Result(b, e))` |
| `filter` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `filter_ok` | `(Channel(Result(a, e)), (a) -> Bool) -> Channel(Result(a, e))` |
| `flat_map` | `(Channel(a), (a) -> List(b)) -> Channel(b)` |
| `take` | `(Channel(a), Int) -> Channel(a)` |
| `drop` | `(Channel(a), Int) -> Channel(a)` |
| `take_while` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `drop_while` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `chunks` | `(Channel(a), Int) -> Channel(List(a))` |
| `scan` | `(Channel(a), b, (b, a) -> b) -> Channel(b)` |
| `dedup` | `(Channel(a)) -> Channel(a)` |
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
| `fold` | `(Channel(a), b, (b, a) -> b) -> b` |
| `each` | `(Channel(a), (a) -> ()) -> ()` |
| `count` | `(Channel(a)) -> Int` |
| `first` | `(Channel(a)) -> Option(a)` |
| `last` | `(Channel(a)) -> Option(a)` |
| `write_to_file` | `(Channel(Bytes), String) -> Result((), String)` |
| `write_to_tcp` | `(Channel(Bytes), TcpStream) -> Result((), String)` |

## Examples

### Three-step pipeline

```silt
import stream

fn main() {
  let squares = stream.from_range(1, 100)
    |> stream.filter(fn(n) { n % 2 == 1 })
    |> stream.map(fn(n) { n * n })
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
  let xs = stream.collect(stream.unfold(1, fn(n) {
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
- **Backpressure is automatic.** When the output channel of a transform
  fills up, the pump thread sleeps briefly and retries — back-pressuring
  into the input channel by not consuming further messages.
- **Errors flow through the stream.** I/O sources emit
  `Channel(Result(_, String))`. Each chunk can fail independently;
  consumers pattern-match. Use `map_ok` / `filter_ok` to apply
  transformations only to `Ok` values, passing `Err(_)` through unchanged.
- **No async/await.** Everything runs on OS threads or the silt scheduler
  via the existing cooperative-I/O machinery.
- **`stream.repeat` is infinite.** Always pair it with `take`,
  `take_while`, or another bounded sink — `collect` on an unbounded
  stream will hang.

## Forward compatibility

Function names mirror what method-form dispatch (`s.map(f)`) would look
like once silt grows a `Stream` trait. Existing v0.10 silt programs will
continue to compile and behave identically when that trait lands.
