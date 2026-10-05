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

## Variants of builtin modules

The enums of a builtin module are members of the module like its
functions: reach a variant through the module, or import it by name.

```silt
import channel
import list
import list.{ Stop }

fn main() {
    let ch = channel.new(1)
    channel.send(ch, 1)
    match channel.receive(ch) {
        channel.Message(v) -> println(v)
        _ -> println("no value")
    }
    println(list.fold_until([1, 2, 3], 0) { acc, x -> Stop(acc + x) })
}
```

| Name | Signature | Description |
|------|-----------|-------------|
| `list.Stop` | `(a) -> list.Step(a)` | Signal early termination in `list.fold_until` |
| `list.Continue` | `(a) -> list.Step(a)` | Signal continuation in `list.fold_until` |
| `channel.Message` | `(a) -> channel.ChannelResult(a)` | Wraps a received channel value |
| `channel.Closed` | `channel.ChannelResult(a)` | Channel is closed |
| `channel.Empty` | `channel.ChannelResult(a)` | Channel buffer empty (non-blocking receive) |
| `channel.Sent` | `channel.ChannelResult(a)` | Result variant for a completed `channel.select` send arm |
| `channel.Recv` | `(Channel(a)) -> channel.ChannelOp(a)` | Build a receive arm for `channel.select` |
| `channel.Send` | `(Channel(a), a) -> channel.ChannelOp(a)` | Build a send arm for `channel.select` |
| `time.Monday`..`time.Sunday` | `time.Weekday` | Day-of-week constructors |
| `http.GET`, `http.POST`, ... | `http.Method` | HTTP method constructors |

The error enums of the builtin modules (`io.IoError`, `json.JsonError`,
...) and their variants (`io.IoNotFound(path)`, ...) are reached the
same way.


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
list.Stop(value: a) -> list.Step(a)
```

A variant of `list`: write `list.Stop(acc)`, or `import list.{ Stop }`.
Signals early termination from `list.fold_until`. The value becomes the final
accumulator result.

```silt
import list
fn main() {
    let capped_sum = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        match {
            acc + x > 6 -> list.Stop(acc)
            _ -> list.Continue(acc + x)
        }
    }
    println(capped_sum)  -- 6
}
```


## `Continue`

```
list.Continue(value: a) -> list.Step(a)
```

A variant of `list`: write `list.Continue(acc)`, or `import list.{ Continue }`.
Signals continuation in `list.fold_until`. The value becomes the next
accumulator.


## `Message`

```
channel.Message(value: a) -> channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Message(v)`, or
`import channel.{ Message }`. Wraps a value received from a channel. Returned by `channel.receive` and
`channel.try_receive` when a value is available.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    when let channel.Message(v) = channel.receive(ch) else { return }
    println(v)  -- 42
}
```


## `Closed`

```
channel.Closed : channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Closed`.

Indicates the channel has been closed. Returned by `channel.receive` and
`channel.try_receive` when no more messages will arrive.


## `Empty`

```
channel.Empty : channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Empty`.

Indicates the channel buffer is currently empty but not closed. Only returned by
`channel.try_receive` (the non-blocking variant).


## `Sent`

```
channel.Sent : channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Sent`. Indicates a successful send
operation inside `channel.select`. When a select arm is built with
`channel.Send(ch, value)` (a `channel.ChannelOp(a)` value), the matching
tuple result is `(ch, channel.Sent)` once that send completes.
`channel.Recv(ch)` arms still produce `channel.Message(v)` /
`channel.Closed`; `Sent` is the send-side counterpart to `Message`. See
`channel.select` in [channel / task](./channel-task.md) for the mixed
send/receive form.


## `Recv`

```
channel.Recv(ch: Channel(a)) -> channel.ChannelOp(a)
```

A variant of `channel`: write `channel.Recv(ch)`. Builds a receive arm for
`channel.select`.


## `Send`

```
channel.Send(ch: Channel(a), value: a) -> channel.ChannelOp(a)
```

A variant of `channel`: write `channel.Send(ch, value)`. Builds a send arm
for `channel.select`.
