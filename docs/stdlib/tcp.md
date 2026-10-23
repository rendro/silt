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
| `Closed` | — | the connection is closed: by `tcp.close`, or by the other end, however the system reports that (a reset, an abort, a broken pipe, an end of data in the middle of a `read_exact`) |
| `Timeout` | — | op exceeded its deadline |
| `Other(msg)` | `String` | unclassified socket failure |

`TcpError` implements the built-in `Error` trait, so `e.message()`
renders any variant as a string when you don't want to branch on it.

## Echo server example

```silt
import bytes
import task
import tcp
import time

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
            println("accept error: {Error.message(e)}")
            loop(())
          }
        }
      }
    }
    Err(e) -> println("listen error: {Error.message(e)}")
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
larger `max` asks for no more. `tcp.read_exact(conn, n)` reads until it
has `n` bytes; a connection that ends before that is `Err(Closed)`.
Neither sets memory aside for the number asked for, only for the bytes
that arrive. The same holds for a chunk of `stream.tcp_chunks`.

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
with the connection. The same holds for the
`stream.tcp_*` sources and sink when their pipeline is cut short or
their I/O times out. A `tcp.accept` that nobody waits
for is woken and gives up; the listener stays usable. A handshake of
`tcp.connect_tls` or `tcp.tls_server` that nobody waits for any more
ends its connection the same way. (The TCP connect of `tcp.connect_tls`,
before its handshake, cannot be interrupted, as `tcp.connect` cannot: it
runs to its end, and its connection is then shut down.) (The whole rule, for every module:
[the I/O pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for).)

## Notes

- silt does not use async/await. Blocking calls run on the same I/O pool as
  `io.read_file`, `fs.list_dir`, etc. (see
  [the I/O pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for)).

## TLS (opt-in feature)

The `tcp-tls` Cargo feature adds TLS support via `rustls`. Build silt with
`--features tcp-tls` to enable.

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
  -- (Build silt with `--features tcp-tls` for these functions.)
  match tcp.connect_tls("example.com:443", "example.com") {
    Ok(conn) -> {
      let _ = tcp.write(conn, bytes.from_string("hello"))
      tcp.close(conn)
    }
    Err(e) -> println("connect_tls err: {Error.message(e)}")
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
    Ok(Err(e)) -> println("handshake failed: {Error.message(e)}")
    Err(_) -> println("the client took too long")
  }
}

fn main() {
  -- (Build silt with `--features tcp-tls` for this function.)
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
      Err(e) -> println("accept failed: {Error.message(e)}")
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
  -- clients. (Build silt with `--features tcp-tls` for this function.)
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
    Err(e) -> println("mTLS handshake failed: {Error.message(e)}")
  }
}
```
