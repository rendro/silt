---
title: "Globals"
section: "Standard Library"
order: 1
---

# Globals

## Always Available

No import or qualification needed.

| Name | Signature | Description |
|------|-----------|-------------|
| `print` | `(a) -> () where a: Display` | Print a value without trailing newline |
| `println` | `(a) -> () where a: Display` | Print a value with trailing newline |
| `panic` | `(a) -> b where a: Display` | Crash with an error message |
| `Ok` | `(a) -> Result(a, e)` | Construct a success Result |
| `Err` | `(e) -> Result(a, e)` | Construct an error Result |
| `Some` | `(a) -> Option(a)` | Construct a present Option |
| `None` | `Option(a)` | The absent Option value (not a function) |

Additionally, four **type descriptors** are in the global namespace for use with
`json.parse_map` and similar type-directed APIs:

| Name | Description |
|------|-------------|
| `Int` | Integer type descriptor |
| `Float` | Float type descriptor |
| `String` | String type descriptor |
| `Bool` | Boolean type descriptor |

## Available After Import

These constructors become available after importing their respective modules.
No module qualification is needed once imported.

| Name | Signature | Import | Description |
|------|-----------|--------|-------------|
| `Stop` | `(a) -> Step(a)` | `import list` | Signal early termination in `list.fold_until` |
| `Continue` | `(a) -> Step(a)` | `import list` | Signal continuation in `list.fold_until` |
| `Message` | `(a) -> ChannelResult(a)` | `import channel` | Wraps a received channel value |
| `Closed` | `ChannelResult(a)` | `import channel` | Channel is closed |
| `Empty` | `ChannelResult(a)` | `import channel` | Channel buffer empty (non-blocking receive) |
| `Sent` | `ChannelResult(a)` | `import channel` | Reserved for future select-send support |
| `Monday`..`Sunday` | `Weekday` | `import time` | Day-of-week constructors |
| `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS` | `Method` | `import http` | HTTP method constructors |


## `print`

```
print(value: a) -> () where a: Display
```

Prints a value to stdout. Does not append a newline. Accepts a single value that
implements `Display`.

```silt
fn main() {
    print("hello ")
    print("world")
    -- output: hello world
}
```


## `println`

```
println(value: a) -> () where a: Display
```

Prints a value to stdout followed by a newline. Accepts a single value that
implements `Display`.

```silt
fn main() {
    println("hello, world")
    -- output: hello, world\n
}
```


## `panic`

```
panic(value: a) -> b where a: Display
```

Terminates execution with an error message. Accepts any value that implements
`Display`. The return type is polymorphic because `panic` never returns -- it
can appear anywhere a value is expected.

```silt
-- noexec
fn main() {
    panic("something went wrong")
    panic(42)  -- also valid
}
```


## `Ok`

```
Ok(value: a) -> Result(a, e)
```

Constructs a success variant of `Result`.

```silt
fn main() {
    let r = Ok(42)
    -- r is Result(Int, e)
}
```


## `Err`

```
Err(error: e) -> Result(a, e)
```

Constructs an error variant of `Result`.

```silt
fn main() {
    let r = Err("not found")
    -- r is Result(a, String)
}
```


## `Some`

```
Some(value: a) -> Option(a)
```

Constructs a present variant of `Option`.

```silt
fn main() {
    let x = Some(42)
    match x {
        Some(n) -> println(n)
        None -> println("nothing")
    }
}
```


## `None`

```
None : Option(a)
```

The absent variant of `Option`. This is a value, not a function.

```silt
import option
fn main() {
    let x = None
    println(option.is_none(x))  -- true
}
```


## `Stop`

```
Stop(value: a) -> Step(a)
```

Signals early termination from `list.fold_until`. The value becomes the final
accumulator result.

```silt
import list
fn main() {
    let capped_sum = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        match {
            acc + x > 6 -> Stop(acc)
            _ -> Continue(acc + x)
        }
    }
    println(capped_sum)  -- 6
}
```


## `Continue`

```
Continue(value: a) -> Step(a)
```

Signals continuation in `list.fold_until`. The value becomes the next
accumulator.


## `Message`

```
Message(value: a) -> ChannelResult(a)
```

Wraps a value received from a channel. Returned by `channel.receive` and
`channel.try_receive` when a value is available.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    when let Message(v) = channel.receive(ch) else { return }
    println(v)  -- 42
}
```


## `Closed`

```
Closed : ChannelResult(a)
```

Indicates the channel has been closed. Returned by `channel.receive` and
`channel.try_receive` when no more messages will arrive.


## `Empty`

```
Empty : ChannelResult(a)
```

Indicates the channel buffer is currently empty but not closed. Only returned by
`channel.try_receive` (the non-blocking variant).


## `Sent`

```
Sent : ChannelResult(a)
```

Indicates a successful send operation inside `channel.select`. When a select
arm is a `(channel, value)` tuple (the send form), the matching tuple result
is `(channel, Sent)` once that send completes. Receive arms still produce
`Message(v)` / `Closed` as usual; `Sent` is the send-side counterpart to
`Message`. See `channel.select` in [channel / task](./channel-task.md) for
the mixed send/receive form.
