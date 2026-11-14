---
title: "tcp"
section: "Standard Library"
order: 16
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
| `accept` | `(TcpListener) -> Result(TcpStream, TcpError)` | Wait for an incoming connection (cooperative I/O) |
| `close` | `(TcpStream) -> ()` | Shut the connection down: operations in flight on it return, later ones give `Err(Closed)` |
| `close_listener` | `(TcpListener) -> ()` | Stop listening: later connects are refused, a waiting accept gives `Err(Closed)`, `http.serve` on it answers what is in flight and returns |
| `connect` | `(String) -> Result(TcpStream, TcpError)` | Open a TCP connection to `host:port` (cooperative I/O) |
| `listen` | `(String) -> Result(TcpListener, TcpError)` | Bind a TCP listener to `host:port` |
| `local_port` | `(TcpListener) -> Int` | The port the listener is bound to: the one the system chose for port `0` |
| `peer_addr` | `(TcpStream) -> Result(String, TcpError)` | The address of the other end, as `ip:port` |
| `read` | `(TcpStream, Int) -> Result(Bytes, TcpError)` | Read up to `max` bytes (cooperative) |
| `read_exact` | `(TcpStream, Int) -> Result(Bytes, TcpError)` | Read exactly `n` bytes (cooperative; loops) |
| `set_nodelay` | `(TcpStream, Bool) -> Result((), TcpError)` | Send small writes at once (`true`) instead of gathering them (Nagle's algorithm, the default) |
| `write` | `(TcpStream, Bytes) -> Result((), TcpError)` | Write the entire buffer and flush (cooperative) |

## Errors

Every fallible `tcp.*` call returns `Result(T, TcpError)`. Variants are
narrow by design — the socket failure space is small once you strip
out the OS-specific noise. A variant says what failed:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Connect(msg)` | `String` | `tcp.connect` (or the connection of `tcp.connect_tls`) made no connection: refused, unreachable, no such host |
| `Bind(msg)` | `String` | `tcp.listen` bound no listener: the address is in use, is not this machine's, is no address |
| `Tls(msg)` | `String` | TLS handshake failure |
| `Closed` | — | the connection is closed: by `tcp.close`, or by the other end, however the system reports that (a reset, an abort, a broken pipe, an end of data in the middle of a `read_exact`); for `tcp.accept`, the listener is closed (`tcp.close_listener`) |
| `Timeout` | — | op exceeded its deadline |
| `Other(msg)` | `String` | unclassified socket failure |

A `TcpError` is shown as its message, so `"{e}"` renders any variant as
a string when you don't want to branch on it.

## Echo server example

```silt
import task
import tcp

fn main() {
  match tcp.listen("127.0.0.1:8080") {
    Ok(listener) -> {
      println("listening on 127.0.0.1:8080")
      loop _ = () {
        match tcp.accept(listener) {
          Ok(conn) -> {
            let _ = task.spawn { ->
              match tcp.read(conn, 4096) {
                Ok(buf) -> {
                  let _ = tcp.write(conn, buf)
                  tcp.close(conn)
                }
                Err(_) -> tcp.close(conn)
              }
            }
            loop(())
          }
          Err(e) -> {
            println("accept error: {e}")
            loop(())
          }
        }
      }
    }
    Err(e) -> println("listen error: {e}")
  }
}
```

## Addresses and ports

`tcp.listen("host:port")` binds where it is told:

- `"127.0.0.1:8080"` listens on loopback: only programs on this machine
  can connect. This is the address for development servers.
- `"0.0.0.0:8080"` listens on every network interface: other machines can
  connect.
- Port `0` asks the system for a free port; `tcp.local_port(listener)`
  gives the one it chose:

```silt
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {
    panic("cannot listen")
  }
  println(tcp.local_port(listener) > 0)
}
```

Connections that arrive before the program accepts them wait in the
listener: up to 4,096 of them on Unix (or the system's own limit, if that
is lower), 128 elsewhere. A client beyond that is not refused; its system
tries again, a second or more later.

A listener is also what [`http.serve`](http.md#httpserve) serves on.
While it does, the listener is the server's alone: `tcp.accept` on it
returns `Err(Other("the listener is served by http.serve"))` at once, and
so does an accept that was waiting on it when the server started. When
the task that serves has been cancelled, the listener is the program's
again: the next `tcp.accept` gets the next client.

## Cooperative I/O

`accept`, `connect`, `read`, `read_exact`, and `write` (and, with TLS,
`connect_tls`, `tls_server` and `tls_server_mtls`) wait without
holding up the scheduler: the operation runs on a thread of the I/O pool
and the task (or `main`) waits for its value while other tasks run. From
silt's perspective the call looks synchronous.

**Reading and writing at the same time.** A plain TCP connection can be
read by one task and written by another at once: neither waits for the
other. (Two tasks that read the same connection take turns, as do two
that write.) A TLS connection is different: its reads and writes take
turns, so a task that waits in `tcp.read` holds up a `tcp.write` on the
same TLS connection until the read returns.

**How much a read takes.** `tcp.read(conn, max)` gives what one read
finds, at most `max` bytes and at most 64 KiB (65536 bytes) at once: a
larger `max` asks for no more. An `Ok` of no bytes is the end of the
data: the other end has closed the connection, or it was shut down
while the read waited. `tcp.read_exact(conn, n)` reads until it has `n`
bytes; a connection that ends before that is `Err(Closed)`. Neither
sets memory aside for the number asked for, only for the bytes that
arrive. The same holds for a chunk of `stream.tcp_chunks`. `max` and
`n` must be positive: a size of 0 or less is a runtime error
("tcp.read: max must be positive", "tcp.read_exact: n must be
positive"), as a chunk size of 0 or less is for `stream.tcp_chunks`.

**How much the peer can send at once.** What comes from the network is
limited, so that a peer does not decide how much memory a program takes:
`tcp.read_exact` takes at most 10,000,000 bytes, and a larger `n` is
`Err(Other("tcp.read_exact: 10000001 bytes is more than the 10000000 one
read may take"))`, with nothing read and the connection as it was (a
length the peer sent is checked so). A line of `stream.tcp_lines` is at
most 10,000,000 bytes too: a longer one is an `Err(Other("a line is
longer than 10000000 bytes"))` item and the last, and the source shuts
its connection down. (A file and stdin have no such limit.)

## Stream lifetime

`tcp.close(conn)` shuts the connection down: a `read` or `write` that
another task has in flight on it returns, and later calls fail with
`Closed`. A closed connection answers nothing: `tcp.peer_addr` and
`tcp.set_nodelay` give `Err(Closed)` too. Without `tcp.close` the socket is closed when the last
reference to the connection is gone.

**A read or write that nobody waits for ends its connection.** When a
task stops waiting for `tcp.read`, `tcp.read_exact` or `tcp.write`
(`SILT_IO_TIMEOUT` passed, the task was cancelled or was the function of
a `task.deadline` whose time is up, or the program ended), the
connection is shut down exactly as by `tcp.close`. That is what lets the
blocked operation, and its thread, end. So a read that timed out cannot
be tried again: the next call on that connection gives `Err(Closed)`.
So is the connection of an operation whose result arrives in the very
moment in which its task stops waiting: what the read had taken is gone
with the connection. The same holds for `stream.write_to_tcp` when its
task is cancelled or its I/O times out. A `stream.tcp_lines` or
`stream.tcp_chunks` source is different: it holds the connection as any
other name does, and when its stream is stopped it lets go of the
connection without shutting it (a read of it that times out shuts it, as
above); the connection closes when its last holder is gone (see
[stream](stream.md)). A `tcp.accept` that nobody waits
for is woken and gives up; the listener stays usable. A `tcp.connect`,
and a connect or handshake of `tcp.connect_tls` or `tcp.tls_server`,
that nobody waits for any more ends its connection the same way. A
connect that still waits for the peer to answer is shut down: on Linux
that ends it at once; elsewhere it may run on to the system's connect
time, counted until then among the I/O operations that cannot be
interrupted, whose number is bounded. (The name lookup of a connect to
a host name cannot be interrupted: it ends by the system's own time,
and the connect gives up after it.) (The whole rule, for every module:
[the I/O pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for).)

## Listener lifetime

`tcp.close_listener(listener)` stops listening. A `tcp.accept` that
waits on the listener returns `Err(Closed)`, and so does every later
one; an `http.serve` on it answers the requests in flight and returns
`()` (see [Stopping a server](http.md#stopping-a-server)). Clients that had
connected but were not yet accepted have their connections closed, and a
client that connects afterwards is refused at once
(`Err(Connect(...))`, "connection refused"), instead of waiting for its
own timeout. The port is free again: `tcp.listen` can bind it. Closing a
closed listener does nothing, and `tcp.local_port` still says the port it
had. Without `tcp.close_listener` a listener listens until the last
reference to it is gone, also after an `http.serve` on it has ended.

## Notes

- silt does not use async/await. Blocking calls run on the same I/O pool as
  `io.read_file`, `fs.list_dir`, etc. (see
  [the I/O pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for)).
- A line of `stream.tcp_lines` that is not UTF-8 is an `Err(Other(..))`
  item and the last: the source ends there and shuts the connection
  down, as `stream.file_lines` ends at such a line of a file.

## TLS

TLS support comes from `rustls`, in the `tcp-tls` Cargo feature, which
every build of silt has unless it is built without its default features.

| Function | Signature | Description |
|----------|-----------|-------------|
| `connect_tls` | `(String, String) -> Result(TcpStream, TcpError)` | Open a TCP connection then complete the TLS client handshake against `hostname` |
| `tls_server` | `(TcpStream, Bytes, Bytes) -> Result(TcpStream, TcpError)` | Run the TLS server handshake on an accepted connection with the PEM cert chain + key; `conn` is taken (cooperative I/O) |
| `tls_server_mtls` | `(TcpStream, Bytes, Bytes, Bytes) -> Result(TcpStream, TcpError)` | Like `tls_server`, but the client must present a cert chaining to the CA PEM bundle (mutual TLS) |

Returned `TcpStream` handles are interchangeable with plain TCP streams —
`tcp.read`, `tcp.write`, and `tcp.close` work identically. Trust anchors
for `connect_tls` come from the `webpki-roots` crate (Mozilla CA bundle).
Authentication is delegated to your system: silt does not add a separate
credential layer.

```silt
import bytes
import tcp

fn main() {
  -- Open a TLS-protected connection and echo a small payload.
  match tcp.connect_tls("example.com:443", "example.com") {
    Ok(conn) -> {
      let _ = tcp.write(conn, bytes.from_string("hello"))
      tcp.close(conn)
    }
    Err(e) -> println("connect_tls err: {e}")
  }
}
```

### A TLS server

A TLS server accepts plain connections with `tcp.accept` and runs the
handshake with `tcp.tls_server(conn, cert, key)`, which returns the TLS
connection. The handshake is an operation like a read: it waits on the
client, so run it in the connection's own task, and the accept loop takes
the next client at once, whatever the last one does. A client that
connects and says nothing holds up its own task only.

`tcp.tls_server` takes the connection it is given: from the call on, that
handle is closed (a `tcp.read` or `tcp.write` of it gives `Err(Closed)`,
and `tcp.close` of it does nothing to the TLS connection), whether the
handshake succeeds or not. Do not read or write it from another task
meanwhile. The result is the only connection to the client:

- `Ok(stream)`: the handshake is done; read and write `stream`.
- `Err(Tls(msg))`: the cert chain or key does not parse, or the handshake
  failed (the client speaks no TLS, or gave up). The connection is shut
  down. A connection that speaks TLS already (from `tcp.connect_tls` or
  `tcp.tls_server`) gives `Err(Tls("the connection speaks TLS already"))`.
- `Err(Closed)`: the connection was closed before the call.
- `Err(Timeout)`: `SILT_IO_TIMEOUT` passed during the handshake.

A handshake that nobody waits for any more (its task was cancelled, or
was the function of a `task.deadline` whose time is up) shuts its
connection down, as a read does: its thread ends.

```silt
import bytes
import io
import task
import tcp
import time

fn serve(conn, cert, key) {
  -- A client gets 10 seconds to complete the handshake.
  match task.deadline(time.seconds(10), { -> tcp.tls_server(conn, cert, key) }) {
    Ok(Ok(tls)) -> {
      let _ = tcp.write(tls, bytes.from_string("hello over TLS\n"))
      tcp.close(tls)
    }
    Ok(Err(e)) -> println("handshake failed: {e}")
    Err(_) -> println("the client took too long")
  }
}

fn main() {
  when let Ok(cert_text) = io.read_file("server.crt") else {
    panic("no cert")
  }
  when let Ok(key_text) = io.read_file("server.key") else {
    panic("no key")
  }
  when let Ok(listener) = tcp.listen("0.0.0.0:8443") else {
    panic("cannot listen")
  }
  let cert = bytes.from_string(cert_text)
  let key = bytes.from_string(key_text)
  loop _ = () {
    match tcp.accept(listener) {
      Ok(conn) -> {
        let _ = task.spawn { -> serve(conn, cert, key) }
        loop(())
      }
      Err(e) -> println("accept failed: {e}")
    }
  }
}
```

### Mutual TLS (mTLS)

`tls_server_mtls` adds client-certificate verification on top of
`tls_server`. The fourth argument is a PEM-encoded bundle of CA
certificates — every connecting client must present a certificate that
chains to one of those CAs, or the TLS handshake fails and the call
returns `Err(Tls(msg))`. This is appropriate for service-to-service
APIs, internal mesh traffic, and any flow where you want cryptographic
client identity rather than bearer tokens.

Under the hood the server uses rustls'
`WebPkiClientVerifier::builder(roots).build()`, which requires
authentication by default (anonymous clients are rejected).

```silt
import bytes
import io
import tcp

fn main() {
  -- Load the server identity and the CA bundle that signs your
  -- clients.
  when let Ok(cert) = io.read_file("server.crt") else {
    panic("no cert")
  }
  when let Ok(key) = io.read_file("server.key") else {
    panic("no key")
  }
  when let Ok(client_ca) = io.read_file("clients-ca.crt") else {
    panic("no CA bundle")
  }
  when let Ok(listener) = tcp.listen("0.0.0.0:8443") else {
    panic("cannot listen")
  }
  when let Ok(conn) = tcp.accept(listener) else {
    panic("accept failed")
  }
  match tcp.tls_server_mtls(
    conn,
    bytes.from_string(cert),
    bytes.from_string(key),
    bytes.from_string(client_ca),
  ) {
    Ok(tls) -> {
      -- Peer is authenticated by cert at this point.
      let _ = tcp.write(tls, bytes.from_string("hello, authenticated client"))
      tcp.close(tls)
    }
    Err(e) -> println("mTLS handshake failed: {e}")
  }
}
```
