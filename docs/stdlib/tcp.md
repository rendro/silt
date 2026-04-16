---
title: "tcp"
section: "Standard Library"
order: 17
---

# tcp

Raw TCP listeners and streams. Returns and consumes [`Bytes`](bytes.md) values
for binary I/O. Blocking operations cooperate with silt's task scheduler — a
silt task that calls `tcp.accept` or `tcp.read` yields its slot, letting other
tasks run, until the I/O completes.

The `tcp` feature is enabled by default. To build silt without it, disable
default features in your `Cargo.toml`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `accept` | `(TcpListener) -> Result(TcpStream, String)` | Wait for an incoming connection (cooperative I/O) |
| `close` | `(TcpStream) -> ()` | Mark the stream as closed; future ops error |
| `connect` | `(String) -> Result(TcpStream, String)` | Open a TCP connection to `host:port` (cooperative I/O) |
| `listen` | `(String) -> Result(TcpListener, String)` | Bind a TCP listener to `host:port` |
| `peer_addr` | `(TcpStream) -> Result(String, String)` | Remote socket address (PR-2 stub: returns Err) |
| `read` | `(TcpStream, Int) -> Result(Bytes, String)` | Read up to `max` bytes (cooperative) |
| `read_exact` | `(TcpStream, Int) -> Result(Bytes, String)` | Read exactly `n` bytes (cooperative; loops) |
| `set_nodelay` | `(TcpStream, Bool) -> Result((), String)` | Disable Nagle (PR-2 stub: returns Err) |
| `write` | `(TcpStream, Bytes) -> Result((), String)` | Write the entire buffer and flush (cooperative) |

## Echo server example

```silt
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:8080") {
    Ok(listener) -> {
      println("listening on 127.0.0.1:8080")
      loop {
        match tcp.accept(listener) {
          Ok(conn) -> {
            task.spawn(fn() {
              match tcp.read(conn, 4096) {
                Ok(buf) -> {
                  let _ = tcp.write(conn, buf)
                  tcp.close(conn)
                }
                Err(_) -> tcp.close(conn)
              }
            })
          }
          Err(e) -> println("accept error: {e}")
        }
      }
    }
    Err(e) -> println("listen error: {e}")
  }
}
```

## Cooperative I/O

`accept`, `connect`, `read`, `read_exact`, and `write` integrate with the silt
scheduler: when called inside a `task.spawn`'d task, they submit the I/O to
silt's thread pool and yield the task slot until the operation completes.
Other tasks run in the meantime. From silt's perspective the call looks
synchronous; under the hood it's cooperative.

When called from the main task (no `task.spawn`), the same operations run
synchronously on the calling thread.

## Stream lifetime

`TcpStream` and `TcpListener` are garbage-collected via `Arc` reference
counting. Dropping the last reference closes the underlying socket.
`tcp.close` is a defensive marker — it makes subsequent `read`/`write` calls
fail fast with a clear message instead of attempting I/O on a stream the user
has logically finished with.

## Notes

- v0.9 PR 2 ships the core read/write/accept/connect surface. `peer_addr`
  and `set_nodelay` return Err in PR 2 (they require unwrapping the
  trait-object stream); they are scheduled to be wired up alongside the TLS
  feature in PR 3.
- TLS (`tcp.connect_tls`, `tcp.accept_tls`) is opt-in via the `tcp-tls`
  Cargo feature, shipping in PR 3.
- silt does not use async/await. The scheduler does cooperative yielding via
  the same I/O pool used by `io.read_file`, `fs.list_dir`, etc.
