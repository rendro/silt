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
| `panic` | `(a) -> Never where a: Display` | Crash with an error message |
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
    Some(v) -> println(v)
    None -> println("no value")
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
| `channel.Received` | `(Channel(a), Option(a)) -> channel.Selected(a)` | What a completed `channel.select` receive arm gives |
| `channel.Sent` | `(Channel(a)) -> channel.Selected(a)` | What a completed `channel.select` send arm gives |
| `channel.Recv` | `(Channel(a)) -> channel.ChannelOp(a)` | Build a receive arm for `channel.select` |
| `channel.Send` | `(Channel(a), a) -> channel.ChannelOp(a)` | Build a send arm for `channel.select` |
| `time.Monday`..`time.Sunday` | `time.Weekday` | Day-of-week constructors |
| `http.GET`, `http.POST`, ... | `http.Method` | HTTP method constructors |

The error enums of the builtin modules (`io.IoError`, `json.JsonError`,
...) and their variants (`io.NotFound(path)`, ...) are reached the
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
panic(message: a) -> Never where a: Display
```

Terminates execution with an error message. Accepts any value that implements
`Display`. It never returns: its result is of type `Never`, the type of an expression
that does not return (`return`, `loop(...)`), which goes with every type, so
`panic` can stand wherever a value is expected and ends an `else` block that
must leave. `Never` is not written in an annotation (see [the Never
type](../language/error-handling.md#never-type)).

```silt
-- noexec
fn main() {
  panic("something went wrong")
  panic(42) -- also valid
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
  println(option.is_none(x)) -- true
}
```


## `list.Stop`

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
  println(capped_sum) -- 6
}
```


## `list.Continue`

```
list.Continue(value: a) -> list.Step(a)
```

A variant of `list`: write `list.Continue(acc)`, or `import list.{ Continue }`.
Signals continuation in `list.fold_until`. The value becomes the next
accumulator.


## `channel.Message`

```
channel.Message(value: a) -> channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Message(v)`, or
`import channel.{ Message }`. Wraps a value received from a channel. Returned by
`channel.try_receive` and `channel.receive_timeout` when a value is available
(`channel.receive` gives `Some(value)`).

```silt
import channel

fn main() {
  let ch = channel.new(1)
  channel.send(ch, 42)
  when let channel.Message(v) = channel.try_receive(ch) else {
    return
  }
  println(v) -- 42
}
```


## `channel.Closed`

```
channel.Closed : channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Closed`.

Indicates the channel has been closed and is empty. Returned by
`channel.try_receive` and `channel.receive_timeout` when no more messages will
arrive (`channel.receive` gives `None`).


## `channel.Empty`

```
channel.Empty : channel.ChannelResult(a)
```

A variant of `channel`: write `channel.Empty`.

Indicates the channel buffer is currently empty but not closed. Returned by
`channel.try_receive`, and by `channel.receive_timeout` when nothing came in
time.


## `channel.Received`

```
channel.Received(ch: Channel(a), value: Option(a)) -> channel.Selected(a)
```

A variant of `channel`: write `channel.Received(ch, v)`. What
`channel.select` gives when an arm built with `channel.Recv(ch)` completes:
`channel.Received(ch, Some(value))` for a value, `channel.Received(ch, None)`
when `ch` is closed and empty. See `channel.select` in
[channel / task](./channel-task.md).


## `channel.Sent`

```
channel.Sent(ch: Channel(a)) -> channel.Selected(a)
```

A variant of `channel`: write `channel.Sent(ch)`. What `channel.select` gives
when an arm built with `channel.Send(ch, value)` completes: the value was
handed off. See `channel.select` in [channel / task](./channel-task.md) for
the mixed send/receive form.


## `channel.Recv`

```
channel.Recv(ch: Channel(a)) -> channel.ChannelOp(a)
```

A variant of `channel`: write `channel.Recv(ch)`. Builds a receive arm for
`channel.select`.


## `channel.Send`

```
channel.Send(ch: Channel(a), value: a) -> channel.ChannelOp(a)
```

A variant of `channel`: write `channel.Send(ch, value)`. Builds a send arm
for `channel.select`.
