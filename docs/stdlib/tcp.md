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
| `peer_addr` | `(TcpStream) -> Result(String, String)` | Remote socket address (not yet implemented for trait-object stream handles; returns Err) |
| `read` | `(TcpStream, Int) -> Result(Bytes, String)` | Read up to `max` bytes (cooperative) |
| `read_exact` | `(TcpStream, Int) -> Result(Bytes, String)` | Read exactly `n` bytes (cooperative; loops) |
| `set_nodelay` | `(TcpStream, Bool) -> Result((), String)` | Disable Nagle (not yet implemented for trait-object stream handles; returns Err) |
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
            let _ = task.spawn(fn() {
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

- `peer_addr` and `set_nodelay` currently return Err (they require unwrapping
  the trait-object stream). They will be wired up in a later release.
- silt does not use async/await. The scheduler does cooperative yielding via
  the same I/O pool used by `io.read_file`, `fs.list_dir`, etc.

## TLS (opt-in feature)

The `tcp-tls` Cargo feature adds TLS support via `rustls`. Build silt with
`--features tcp-tls` to enable.

| Function | Signature | Description |
|----------|-----------|-------------|
| `accept_tls` | `(TcpListener, Bytes, Bytes) -> Result(TcpStream, String)` | Accept a connection and complete the TLS server handshake using the supplied PEM cert chain + key |
| `accept_tls_mtls` | `(TcpListener, Bytes, Bytes, Bytes) -> Result(TcpStream, String)` | Like `accept_tls`, but also requires the client to present a cert chaining to the supplied CA PEM bundle (mutual TLS) |
| `connect_tls` | `(String, String) -> Result(TcpStream, String)` | Open a TCP connection then complete the TLS client handshake against `hostname` |

Returned `TcpStream` handles are interchangeable with plain TCP streams —
`tcp.read`, `tcp.write`, and `tcp.close` work identically. Trust anchors
for `connect_tls` come from the `webpki-roots` crate (Mozilla CA bundle).
Authentication is delegated to your system: silt does not add a separate
credential layer.

```text
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
    Err(e) -> println("connect_tls err: {e}")
  }
}
```

### Mutual TLS (mTLS)

`accept_tls_mtls` adds client-certificate verification on top of
`accept_tls`. The fourth argument is a PEM-encoded bundle of CA
certificates — every connecting client must present a certificate that
chains to one of those CAs, or the TLS handshake fails and the call
returns `Err(msg)`. This is appropriate for service-to-service APIs,
internal mesh traffic, and any flow where you want cryptographic client
identity rather than bearer tokens.

Under the hood the server uses rustls'
`WebPkiClientVerifier::builder(roots).build()`, which requires
authentication by default (anonymous clients are rejected).

```text
import bytes
import io
import tcp

fn main() {
  -- Load the server identity and the CA bundle that signs your
  -- clients. (Build silt with `--features tcp-tls` for this function.)
  match io.read_file("server.crt") {
    Ok(cert) -> match io.read_file("server.key") {
      Ok(key) -> match io.read_file("clients-ca.crt") {
        Ok(client_ca) -> match tcp.listen("0.0.0.0:8443") {
          Ok(listener) -> match tcp.accept_tls_mtls(listener, cert, key, client_ca) {
            Ok(conn) -> {
              -- Peer is authenticated by cert at this point.
              let _ = tcp.write(conn, bytes.from_string("hello, authenticated client"))
              tcp.close(conn)
            }
            Err(e) -> println("mTLS handshake failed: {e}")
          }
          Err(e) -> println("listen err: {e}")
        }
        Err(e) -> println("ca load err: {e}")
      }
      Err(e) -> println("key load err: {e}")
    }
    Err(e) -> println("cert load err: {e}")
  }
}
```
