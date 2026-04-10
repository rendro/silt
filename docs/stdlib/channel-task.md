---
title: "channel / task"
section: "Standard Library"
order: 12
---

# channel

Bounded channels for concurrent task communication. Channels provide
communication between tasks spawned with `task.spawn`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `close` | `(Channel) -> ()` | Close the channel |
| `each` | `(Channel, (a) -> b) -> ()` | Iterate until channel closes |
| `new` | `(Int?) -> Channel` | Create a channel (0 = rendezvous, N = buffered) |
| `receive` | `(Channel) -> ChannelResult(a)` | Blocking receive |
| `select` | `(List(Channel(a))) -> (Channel(a), ChannelResult(a))` | Wait on multiple channels |
| `send` | `(Channel, a) -> ()` | Blocking send |
| `timeout` | `(Int) -> Channel` | Create a channel that closes after N ms |
| `try_receive` | `(Channel) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel, a) -> Bool` | Non-blocking send |


## `channel.close`

```
channel.close(ch: Channel) -> ()
```

Closes the channel. Subsequent sends will fail. Receivers will see `Closed`
after all buffered messages are consumed.

```silt
fn main() {
    let ch = channel.new(10)
    channel.send(ch, 1)
    channel.close(ch)
}
```


## `channel.each`

```
channel.each(ch: Channel, f: (a) -> b) -> ()
```

Receives messages from the channel and calls `f` with each one, until the
channel is closed. This is the idiomatic way to consume all messages.

```silt
fn main() {
    let ch = channel.new(10)
    task.spawn(fn() {
        channel.send(ch, 1)
        channel.send(ch, 2)
        channel.close(ch)
    })
    channel.each(ch) { msg -> println(msg) }
    -- prints 1, then 2
}
```


## `channel.new`

```
channel.new() -> Channel
channel.new(capacity: Int) -> Channel
```

Creates a new channel. With no argument, creates a rendezvous channel
(capacity 0) where the sender blocks until a receiver is ready and vice versa.
With an integer argument, creates a buffered channel with that capacity --
sends block when the buffer is full, receives block when the buffer is empty.

```silt
fn main() {
    let rendezvous = channel.new()    -- true rendezvous (capacity 0)
    let buffered = channel.new(10)    -- buffered (capacity 10)
}
```


## `channel.receive`

```
channel.receive(ch: Channel) -> ChannelResult(a)
```

Receives a value from the channel. Returns `Message(value)` when a value is
available, or `Closed` when the channel is closed and empty. Parks the task
while waiting, allowing other tasks to run on the same thread.

```silt
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    match channel.receive(ch) {
        Message(v) -> println(v)
        Closed -> println("done")
        _ -> unit
    }
}
```


## `channel.select`

```
channel.select(ops: List(Channel(a))) -> (Channel(a), ChannelResult(a))
```

Waits until one of the channels has data or is closed. Takes a list of channels
and returns a 2-tuple of `(channel, result)` where `result` is `Message(val)`
for a successful receive or `Closed` if the channel is closed.

```silt
fn main() {
    let ch1 = channel.new(1)
    let ch2 = channel.new(1)
    task.spawn(fn() { channel.send(ch2, "hello") })
    match channel.select([ch1, ch2]) {
        (^ch2, Message(val)) -> println(val)  -- "hello"
        (_, Closed) -> println("closed")
        _ -> unit
    }
}
```


## `channel.send`

```
channel.send(ch: Channel, value: a) -> ()
```

Sends a value into the channel. Parks the task if the buffer is full, allowing
other tasks to run until space opens up.

```silt
fn main() {
    let ch = channel.new(1)
    channel.send(ch, "hello")
}
```


## `channel.timeout`

```
channel.timeout(ms: Int) -> Channel
```

Creates a channel that automatically closes after the given number of
milliseconds. The returned channel carries no values -- it simply closes when
the duration elapses. This is useful for adding deadlines to `channel.select`.

```silt
fn main() {
    let ch = channel.new(10)
    let timer = channel.timeout(1000)  -- closes after 1 second
    match channel.select([ch, timer]) {
        (^ch, Message(val)) -> println("got: {val}")
        (^timer, Closed) -> println("timed out")
        _ -> unit
    }
}
```


## `channel.try_receive`

```
channel.try_receive(ch: Channel) -> ChannelResult(a)
```

Non-blocking receive. Returns `Message(value)` if a value is immediately
available, `Empty` if the channel is open but has no data, or `Closed` if the
channel is closed and empty.

```silt
fn main() {
    let ch = channel.new(1)
    match channel.try_receive(ch) {
        Message(v) -> println(v)
        Empty -> println("nothing yet")
        Closed -> println("done")
        _ -> unit
    }
}
```


## `channel.try_send`

```
channel.try_send(ch: Channel, value: a) -> Bool
```

Non-blocking send. Returns `true` if the value was successfully buffered,
`false` if the buffer is full or the channel is closed.

```silt
fn main() {
    let ch = channel.new(1)
    let ok = channel.try_send(ch, 42)
    println(ok)  -- true
}
```


---

# task

Spawn and coordinate lightweight concurrent tasks. Tasks are multiplexed onto a
fixed thread pool and run in parallel. They communicate through channels.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `cancel` | `(Handle) -> ()` | Cancel a running task |
| `join` | `(Handle) -> a` | Wait for a task to complete |
| `spawn` | `(() -> a) -> Handle` | Spawn a new lightweight task |


## `task.cancel`

```
task.cancel(handle: Handle) -> ()
```

Cancels a running task. The task will not execute further. No-op if the task has
already completed.

```silt
fn main() {
    let h = task.spawn(fn() {
        -- long-running work
    })
    task.cancel(h)
}
```


## `task.join`

```
task.join(handle: Handle) -> a
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

```silt
fn main() {
    let h = task.spawn(fn() { 1 + 2 })
    let result = task.join(h)
    println(result)  -- 3
}
```


## `task.spawn`

```
task.spawn(f: () -> a) -> Handle
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join` or `task.cancel`.

```silt
fn main() {
    let h = task.spawn(fn() {
        println("running in a task")
        42
    })
    let result = task.join(h)
    println(result)  -- 42
}
```
