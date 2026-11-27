---
title: "http"
section: "Standard Library"
order: 15
---

# http

HTTP client and server. Included by default (the server needs [tcp](tcp.md), which the `http` feature brings with it). Exclude with `--no-default-features` for WASM or minimal builds: a program that names `http.get`, `http.request` or `http.serve` then does not check (the error names the cargo feature `http`), while the module's types remain. A URL is a [`url.Url`](url.md), in a request the client sends and in one the server gets.

## Types

```silt
type Method {
  GET,
  POST,
  PUT,
  PATCH,
  DELETE,
  HEAD,
  OPTIONS,
}

type Request {
  method: Method,
  url: url.Url,
  headers: Map(String, String),
  body: Bytes,
  peer: String,
}

type Response {
  status: Int,
  body: Bytes,
  headers: Map(String, String),
}
```

`Method` and its variants are members of `http`: `http.Method`, `http.GET`, `http.POST`, etc.

A body is [`Bytes`](bytes.md), in a request and in a response, sent and
received: exactly the bytes that are on the wire, whatever they are.
Text is put into a body with `bytes.from_string("...")` and taken out of
one with `bytes.to_string(resp.body)`, a `Result`: it is
`Err(InvalidUtf8(offset))` for a body that is no UTF-8.

```silt
import bytes
import http
import url

-- Text into a body: what a handler answers.
pub fn hello(_req: http.Request) -> http.Response {
  http.Response { status: 200, body: bytes.from_string("Hello"), headers: #{} }
}

-- Text out of a body: what a client got.
fn main() -> Result((), url.UrlError) {
  let home = url.parse("http://127.0.0.1:8080/")?
  match http.get(home) {
    Ok(resp) -> match bytes.to_string(resp.body) {
      Ok(text) -> println(text)
      Err(e) -> println("the body is no text: {e}")
    }
    Err(e) -> println("{e}")
  }
  Ok(())
}
```

The client takes a [`url.Url`](url.md), never a text: a URL is read
(`url.parse`) or built (`url.add_segments`, `url.with_query`) by the url
module, which checks and escapes it, and the client never reads URL text
the program wrote. A value put into a URL with the url module stays one
value: a name holding `/`, `?`, `#`, `&` or `..` asks for no other
resource.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `get` | `(Url) -> Result(Response, HttpError)` | HTTP GET request |
| `request` | `(Method, Url, Bytes, Map(String, String)) -> Result(Response, HttpError)` | HTTP request with method, URL, body, headers |
| `serve` | `(TcpListener, Fn(Request) -> Response) -> ()` | Serve HTTP on a listener made with `tcp.listen`, a task per connection |

## Errors

`http.get` and `http.request` return `Result(Response, HttpError)`. Note
that a 4xx or 5xx HTTP response is an `Ok(Response)`: an `Err` is a
response that did not arrive whole. That is a failure before its head
is there (DNS, connection, TLS, a timeout, a head that is no HTTP), and
one after it: a body that is cut short (`ClosedEarly`), and a body that
does not unpack or is larger than 10,000,000 bytes (`InvalidResponse`).
A program for which a status is a failure looks at `resp.status`.
An `HttpError` is shown as its message, so `"{e}"` always yields a
rendered string.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Connect(msg)` | `String` | no connection was made: refused, no route, no such host |
| `Tls(msg)` | `String` | TLS handshake / cert failure |
| `Timeout` | — | one of the request's times passed (10 s to connect, 60 s in all), or `SILT_IO_TIMEOUT` |
| `InvalidUrl(url)` | `String` | the URL has a scheme other than `http` and `https`, is longer than 65,534 bytes (the transport's limit), or has a user with a `:` in it once decoded; no connection is made. Also the location of a redirect that is no URL or has such a scheme, which is not followed. The URL is shown with its user and password hidden |
| `InvalidResponse(msg)` | `String` | what the server sent is no HTTP (a status line that is none, a chunk length that is no number, `content-length` headers that disagree or a length that is no number, a head that is too large, a body that says it is packed and, though all of it arrived, does not unpack: no bytes at all among them), or its body is larger than 10,000,000 bytes |
| `ClosedEarly` | — | the server went away before the response was complete, head or body, packed or not |
| `Other(msg)` | `String` | any other failure: a header name or value of the request that is none, too many redirects |

Which variant a failure is, is decided by the kind of failure the
transport reports, never by the wording of its message: the message is
only what `Connect`, `Tls`, `InvalidResponse` and `Other` carry.

To bound a request by a time of your own, put a `task.deadline` around
it. A request that nobody waits for any more (its deadline passed, its
task was stopped) is ended at once, wherever it waits: for the
server's answer, in its body, in a TLS handshake, and holds no thread.
So is a connect that waits for a host that does not answer: on Linux it
ends at once; elsewhere it may run on to its connect time, at most 10 s,
counted until then among the I/O operations that cannot be interrupted
(see [the I/O
pool](../concurrency.md#the-io-pool-and-operations-that-nobody-waits-for)).
The name lookup of a host name cannot be interrupted: it ends by the
system's own time.


## `http.get`

```
http.get(u: Url) -> Result(Response, HttpError)
```

Makes an HTTP GET request. Returns `Ok(Response)` for a response of any
status (4xx and 5xx too) that arrived whole, and `Err(HttpError)`
otherwise: see [Errors](#errors).
A redirect is followed as [`http.request`](#httprequest) follows it, up
to ten of them: the `Response` is the last one's, and nothing in it says
where it came from.
The names of the response's headers are in lower case
(`map.get(resp.headers, "content-type")`); a header that came more than
once is one entry, its values joined with `", "` in the order they came
(two `Set-Cookie` headers give `"a=1, b=2"`, which is hard to split
again, since a cookie's date has a comma in it). Its body is the bytes
the server sent, with one exception:

- A body with the header `Content-Encoding: gzip`, written exactly so, is
  unpacked: `resp.body` is the content, and `content-encoding` and
  `content-length`, which describe the packed bytes, are not in
  `resp.headers`. (Every request says `accept-encoding: gzip`.) A body
  in any other coding, `GZIP` and `x-gzip` among them, is handed over as
  it came, with its header. A coding sent in two headers is their values
  joined, as of any header: two `gzip` headers are `gzip, gzip`, and the
  body is handed over as it came.
- An answer to `HEAD`, a `204` and a `304` have no body: `resp.body` is
  empty and nothing is read, whatever the headers say. They may say what
  a `GET` would get; of those, `content-encoding` and `content-length`
  are not in `resp.headers` when the answer says `gzip`.
- A body of more than 10,000,000 bytes (the limit of what one read
  from the network may take, as for `tcp.read_exact`), counted as the
  program would get it, is
  `Err(InvalidResponse(_))`: no part of it is returned.

Every request says who sends it, `user-agent: silt/<version>` (the
version of `silt` that runs the program).

When called from a task, `http.get` transparently yields to the
scheduler while the request is in flight. No API change is needed -- the
call site looks the same.

```silt
import bytes
import http
import url

fn main() {
  match url.parse("https://api.github.com/users/torvalds") {
    Ok(u) -> match http.get(u) {
      Ok(resp) -> println("Status: {resp.status}, body length: {bytes.length(resp.body)}")
      Err(http.Timeout) -> println("timed out; retry later")
      Err(e) -> println("Network error: {e}")
    }
    Err(e) -> println("not a URL: {e}")
  }
}
```

Compose with `url.add_segments`, `bytes.to_string`, `json.parse` and `?`
for typed API responses. Since they return different error types, wrap
each in a local enum using `result.map_err` with a variant constructor as
a first-class `Fn`. The user's name is one path segment, escaped: a name
`ann/../admin?x=1` asks for `/v1/users/ann%2F..%2Fadmin%3Fx=1`, one user,
as meant.

```silt
import bytes
import http
import json
import result
import url

type User {
  name: String,
  id: Int,
}

type FetchError {
  BadUrl(url.UrlError),
  Network(http.HttpError),
  NotText(bytes.BytesError),
  Parse(json.JsonError),
}

-- `api` is the base of the API, `https://api.example.com/v1/`.
fn fetch_user(api: url.Url, name: String) -> Result(User, FetchError) {
  let u = api |> url.add_segments(["users", name]) |> result.map_err(BadUrl)?
  let resp = http.get(u) |> result.map_err(Network)?
  let text = bytes.to_string(resp.body) |> result.map_err(NotText)?
  json.parse(text, User) |> result.map_err(Parse)
}
```


## `http.request`

```
http.request(method: Method, u: Url, body: Bytes, headers: Map(String, String)) -> Result(Response, HttpError)
```

Makes an HTTP request with full control over method, body, and headers. Use this for POST, PUT, DELETE, or any request that needs custom headers.

The body is sent as it is, with its length: `bytes.from_string("...")` for
a text, `bytes.empty()` for none. What the bytes are is the request's to
say, in a `Content-Type` header among `headers`: none is added. A
`user-agent` among `headers` is sent instead of `silt/<version>`, and a
`Host` instead of the URL's host. A `Content-Length`, a
`Transfer-Encoding` and a `Connection` among `headers` are not sent: the
body goes whole, with its own length, and the connection is the client's.

The user and password of the URL (`url.with_user`, `url.with_password`,
which escape them) are sent as `authorization: Basic ...` (RFC 7617),
decoded, unless `headers` has an `authorization` of its own, for
`http.get` too; they are never sent in the URL itself. A user that has a
colon (`us:er`) is an `InvalidUrl`: RFC 7617 has none in it. No error
shows them: the URL of an `InvalidUrl` is shown as `url.Url` is, with
`***@` in their place.

The URL goes out as the `Url` holds it, in its normal form: escaped
(`/h%C3%A9llo` for `/héllo`), its `.` and `..` segments taken out, without
its fragment, which is no server's. A URL longer than 65,534 bytes is an
`InvalidUrl`: the transport takes no longer one.

A redirect is followed, up to ten of them: after a 307 or 308 with the
same method and body; after a 303 as a `GET` without a body and without
the `Content-Type` among `headers` (a `HEAD` stays a `HEAD`); after a 301
or 302 a `POST` as such a `GET`, and any other method with the same
method and body. The credentials are not sent to where a redirect leads,
also when it leads to the same host: the `authorization`,
`proxy-authorization` and `cookie` headers, and the user and password of
the URL. Every other header among `headers` is, and so is the body after
a 307 or 308, also to another host, as curl and Go send them. A location
is resolved against the URL that answered with `url.resolve`: `?y=2` on
its path, `//host/x` on its scheme, `../x` next to its path. An
empty location, or one that is a fragment alone (`#top`), is the URL that
answered itself, as RFC 3986 reads it: after a 303 that is a `GET` of the
same URL, and a server that answers it so every time ends in
`Err(Other("too many redirects"))`. A response without a location is
handed back as it is, its status 3xx. A byte of a location that a URL
cannot hold (a space, the bytes of `é`) is escaped first (`%20`,
`%C3%A9`), as browsers send it. A location that does not resolve to a URL
(`url.resolve` refuses it: escapes that are no UTF-8 text, a host that is
none, `mailto:a@b`), or that resolves to one of a scheme other than
`http` and `https` (`file:///x`, `ftp://host/x`), is not followed: it is
`Err(InvalidUrl(location))`, a user and password in it hidden.

Like `http.get`, this transparently yields to the scheduler when called from
a task.

```silt
-- `api` is a Url, `https://api.example.com/`.

-- POST with JSON body
let users = api |> url.add_segments(["users"])?
let resp = http.request(
  http.POST,
  users,
  bytes.from_string(json.stringify(#{ "name": "Alice" })),
  #{ "Content-Type": "application/json", "Authorization": "Bearer tok123" },
)?

-- DELETE
let user = api |> url.add_segments(["users", "42"])?
let resp = http.request(http.DELETE, user, bytes.empty(), #{})?

-- GET with a query and custom headers
let data = api |> url.add_segments(["data"])?
let resp = http.request(
  http.GET,
  data |> url.with_query([("page", "2")]),
  bytes.empty(),
  #{ "Accept": "text/plain" },
)?
```


## `http.serve`

```
http.serve(listener: TcpListener, handler: Fn(Request) -> Response) -> ()
```

Serves HTTP on a listener made with [`tcp.listen`](tcp.md). The address
written there decides where the server is reached, and on which port:

| `tcp.listen(...)` | The server is reached |
|---|---|
| `"127.0.0.1:8080"` | from this machine only (loopback). **Write this unless you mean otherwise**: a development server is then not exposed to the network the machine happens to be on. |
| `"0.0.0.0:8080"` | on every network interface: from the LAN, and from the internet if the host is routed. For a deployment behind a reverse proxy, or a container whose port is published. |
| `"127.0.0.1:0"` | on a port the system chooses; [`tcp.local_port(listener)`](tcp.md) says which. For tests, and for servers that tell someone else where they are. |

The listener is bound when `tcp.listen` returns, so a client may connect
before `http.serve` runs: its request is queued and answered when the
server starts.

`http.serve` serves until its listener is closed or its task is stopped.
To run a server beside other work, make it a helper of
`task.background` or a piece of `task.join`. There are two ways to stop
it (see [Stopping a server](#stopping-a-server)):
`tcp.close_listener(listener)` frees the port and lets the server finish
what it has begun, then `http.serve` returns `()`; a stop of its task
(the body of the `task.background` returned, a `task.deadline` around it
passed) ends the server at once and leaves the listener bound, to be
served again (see [When a program ends](../concurrency.md#when-a-program-ends)).

```silt
import bytes
import http
import tcp
import url

fn main() -> Result((), tcp.TcpError) {
  let listener = tcp.listen("127.0.0.1:8080")?
  http.serve(listener) { req ->
    http.Response {
      status: 200,
      body: bytes.from_string("you asked for {url.path(req.url)}"),
      headers: #{},
    }
  }
  Ok(())
}
```

### Connections and handlers

Each connection has a task of its own. It reads a request, calls the
handler with it, sends what the handler returns, and reads the next
request of the connection. So:

- Requests on different connections are handled concurrently.
- Requests on one connection are answered one after the other, in their
  order: a connection is kept after a response (keep-alive), and a client
  may send several requests without waiting for the answers.
- A handler that waits (for a channel, a timer, another request: a long
  poll) holds no thread while it does.

The handler receives a `Request` and must return a `Response` whose
`status` is the status of a response: 200 to 999.

| What happens | What the client gets |
|---|---|
| The handler fails, or returns a status that is none | `500`. The failure is written to stderr as one line (a line end in its text shown as `\n`) and nothing of it is sent. The connection stays usable. |
| 128 handlers are being called already | The request waits for one to return, in the server's queue, first come first served: a short burst is answered as the handlers free up. A request that has waited 5 s, or one that finds 1,024 waiting already, is answered `503`. A request that would release the handlers that wait is queued like any other: a server whose handlers wait for each other needs a second server, or a timeout in the handler. |
| The method is none of `Method`'s (e.g. `TRACE`) | `405`, with an `Allow` header that names the seven |
| The target or the `Host` makes no URL, or one that two readers could take differently (see `req.url` below): a `Host` that is more than a host and a port, or whose port is empty (`h.test:`); a fragment in the target (`/x#top`); an escape that is none or no UTF-8 text; a target in absolute form of another scheme than `http`, with a user or password, or whose host and port are not the `Host` header's | `400`; the handler is not called. The connection stays usable. |
| `OPTIONS *`, which asks the server and no resource | `204`, with the `Allow` header of a `405`; the handler is not called |
| The method is `HEAD` | the handler is called; its response is sent without the body, with the length the body has |

**A client that leaves is not noticed while its handler runs.** The
server looks at a connection when it reads from it or writes to it. If a
client closes its connection while its handler waits (a long poll), the
handler goes on waiting: it keeps its place among the 128, and the
connection its descriptor, until the handler returns and the response
cannot be sent. Clients that come and go without waiting for their
answers can so fill every place, and every later request waits its 5 s
in the queue and gets `503`. A
handler that waits needs a limit of its own, for example
`channel.receive_timeout` instead of `channel.receive`.

**Long polls and health checks: serve them on two listeners.** Every
request to one server takes a place among its 128 handlers, so 128 long
polls that wait hold every place, and a health check (`GET /health`) waits
in the queue behind them until one returns, or is answered `503` after
5 s. A request that must
be answered at once while others wait goes to a second server on another
port, which has 128 places of its own:

```silt
import bytes
import http
import task
import tcp

fn text(body: String) -> http.Response {
  http.Response { status: 200, body: bytes.from_string(body), headers: #{} }
}

fn main() -> Result((), tcp.TcpError) {
  let api = tcp.listen("127.0.0.1:8080")?
  let health = tcp.listen("127.0.0.1:8081")?
  let checks = { -> http.serve(health) { _ -> text("up") } }
  -- the long polls, and every other request, are served on 8080
  task.background([checks]) { -> http.serve(api) { _ -> text("done") } }
  Ok(())
}
```

### What the server reads and sends

The server speaks HTTP/1.1, and HTTP/1.0 to a client that does (the
connection is then closed after the response unless the request says
`Connection: keep-alive`). A request body is given by `Content-Length`
or sent in chunks (`Transfer-Encoding: chunked`; trailers are read and
dropped); `req.body` is the body's bytes, whichever way they came. A client that sends `Expect:
100-continue` is told to go on before its body is read. `Connection:
close` closes the connection after the response.

A response is sent with `Content-Length`, `Date` and, where the
connection ends with it, `Connection: close`. Those three, and
`Transfer-Encoding`, are the server's: headers of these names in
`Response.headers` are left out (the body is sent whole, with its length), and
so is a header whose name or value could not stand in one header line.
The body is the bytes of `Response.body`, as they are, and what they are
is the handler's to say: a response has the `Content-Type` that
`Response.headers` gives it, and none if the handler sets none. (The
server's own answers, such as a `405` or a `503`, are a word of text and
say `text/plain; charset=UTF-8`.) There is no `Server` header.

### Limits

| Limit | | Beyond it |
|---|---|---|
| The head of a request: request line and headers | 64 KiB | `431`, connection closed |
| Headers of a request | 100 | `431`, connection closed |
| The body of a request | 10,000,000 bytes | `413`, connection closed |
| The body of a response that `http.get` or `http.request` reads (the client's one limit of size), counted after it is unpacked | 10,000,000 bytes | `Err(InvalidResponse("the body is larger than 10000000 bytes"))`; no more than one byte beyond the limit is read |
| The bodies, together, of the requests that no handler has yet: those being read (a body counts from when its length is known), those that wait in the queue for a handler, and those about to be handed over | 256 MiB | `503`, connection closed: the request whose body would not fit is refused before its body is read. With every handler holding a request of the largest size besides, the server holds 128 x 10,000,000 bytes more: about 1.5 GB of bodies is the worst case. |
| A method | 32 bytes | `400`, connection closed |
| A line of a chunked body: a chunk's size and its extensions | 4 KiB | `400`, connection closed |
| What of a chunked body is not the body: chunk sizes, extensions, line ends | 1 MiB | `400`, connection closed. (A body of 10,000,000 bytes in chunks of 100 bytes is within it; a body cut into single bytes is not.) |
| The trailers of a chunked body | 64 KiB | `431`, connection closed |
| Handlers being called | 128 | the request waits in the queue |
| Requests that wait for a handler | 1,024 | `503` at once |
| Time a request waits for a handler | 5 s | `503` |
| Time for the head of a request, from when the server starts to wait for it. This is also how long a kept connection that sends nothing stays open. | 30 s | `408` if part of the request has come, and the connection closed; a connection that sent nothing, or only an empty line, is closed without a word |
| Time for the body of a request to arrive after its head | 5 min | `408`, connection closed |
| Time for a response to be taken by the client | 5 min | connection closed |
| Time the server goes on reading (and dropping) what a client still sends after a refusal or a `408`, so that the answer reaches it | 5 s | connection closed |

A request that two readers could take differently is refused with `400`,
never guessed at: `Content-Length` together with `Transfer-Encoding`;
`Content-Length` twice, or not a plain number; a transfer coding other
than `chunked`; a chunk size that is no hexadecimal number or does not
fit; a control byte in a chunk's line or in a trailer; a line of the head
that ends in a bare LF, or a CR that ends no line; whitespace before the
colon of a header; a header that goes on in the next line; an HTTP/1.1
request without a `Host` header, and any request with two. After a
refusal the connection is closed, and what followed the refused request
on it is not served. A `#` in the target (`GET /x#top`), which a proxy
in front may read as part of the path, and a target in absolute form
whose host and port are not those of the `Host` header are refused with
`400` too; the request was read whole, so the connection stays usable.

An `Expect` header that asks for anything but `100-continue` is answered
`417`, and the connection closed.

### What the handler gets

- `req.headers` has every name in lower case, however the client wrote
  it: a header's name is not case-sensitive, and a lookup asks for one
  spelling, `map.get(req.headers, "content-type")`. A header sent more
  than once is one entry, its values joined with `", "` in the order
  they came (`x-a: 1` and `x-a: 2` give `"1, 2"`). A header's value is
  text: bytes in it that are no UTF-8 reach the handler as U+FFFD, unlike
  the body, which is exact. (The headers of a `Response` that `http.get`
  or `http.request` returns are the same: names in lower case, values
  joined, bytes that are no UTF-8 as U+FFFD.)
- `req.body` is `Bytes`, exactly what arrived: a binary upload is intact.
  A handler that wants text says so: `bytes.to_string(req.body)` is
  `Err(InvalidUtf8(offset))` for a body that is no UTF-8. The body of the
  `Response` it returns is `Bytes` in the same way, sent exactly.
- `req.url` is the URL the request asks for, a [`url.Url`](url.md):
  `http://`, the host the request names (its `Host` header) and the target
  as sent, in its normal form. Route on `url.segments(req.url)`, the
  path's segments decoded (`/users/a%20b` is `["users", "a b"]`, `%2F` a
  `/` inside one segment, a `/` at the end a last `""`); read the query
  with `url.query(req.url)`, its pairs decoded. `.` and `..` are taken out
  before the handler sees the URL (`/a/../b` arrives as `/b`), and so is
  a needless escape (`%7e` arrives as `~`): the text exactly as the client
  sent it is not kept, so a signature over the raw request line cannot be
  checked. `url.path(req.url)` and `url.query_text(req.url)` give the path
  and the query as they stand in the URL, escaped, for a log line. No
  client sends a fragment, so `url.fragment(req.url)` is `""`: a target
  with a `#` in it is answered `400`. The scheme is always `http`: the
  server speaks no TLS (behind a proxy that does, `x-forwarded-proto` is
  the handler's to read). A target in absolute form
  (`GET http://other.example/x HTTP/1.1`) is that URL. It is taken of
  `http` only (`https://` and every other scheme get `400`), and its `Host`
  header must be one the server takes in a request of its own and name the
  same host and port, the host in any case and port 80 the same as none
  (`Host: Other.Example:80`); otherwise the request gets `400`. So
  `url.host(req.url)` and the `host` header never name two hosts. An
  HTTP/1.0 request without a `Host` has the host of its target in absolute
  form, and otherwise the address the client reached (`127.0.0.1:8080`).
- `req.peer` is the address of the client's end of the connection, as
  `ip:port`: `127.0.0.1:53422`, and an IPv6 address in brackets,
  `[::1]:53422`. An IPv4 client of a listener on `[::]` is written by its
  IPv4 address too, not as `[::ffff:127.0.0.1]:53422`. It is the connection's, so behind a reverse proxy it is
  the proxy's; the client's own address is then in a header the proxy
  sets (such as `x-forwarded-for`). An access log line:
  `io.eprintln("{req.peer} {req.method} {url.path(req.url)}")`.
- If the program fails while requests are in flight, it ends as a
  process: their connections are closed without an answer.

These times are the server's own, and the only ones that bound a
connection's wait for a request, a body or a client: `SILT_IO_TIMEOUT`
does not. Inside a handler it works as in any task: the handler's own
I/O ends at `SILT_IO_TIMEOUT`, and a `task.deadline` that the handler
sets ends what it runs. A `task.deadline` around `http.serve` itself
ends the server when its time is up, as any stop of the task that
serves does: the listener stays bound and can be served again.

Every connection that waits for a request has one I/O operation in
flight, of the 4,096 a program can have (see
[concurrency](../concurrency.md)).

### The listener while it is served

While `http.serve` serves a listener, the listener is the server's alone:

- `tcp.accept` on it returns
  `Err(Other("the listener is served by http.serve"))` at once. An
  accept that was already waiting on the listener when the server started
  returns the same error at that moment: the server has every client.
- A second `http.serve` on it is a runtime error.

### Stopping a server

`tcp.close_listener(listener)` stops a server for good, and lets it
finish what it has begun (a graceful shutdown):

- A client that connects afterwards is refused at once (rather than
  waiting for its own timeout, 60 s for `http.request`), and the port can
  be bound again.
- Every request already in flight is answered: a handler that runs goes
  on to its end and its response is written, and so are the requests that
  wait for a handler and one whose bytes have begun to come. Each of
  these responses says `Connection: close`, and its connection is closed
  after it.
- A kept-alive connection that waits for its next request is closed.
- `http.serve` returns `()` when the last connection has ended.

A stop of the task that serves (the body of the `task.background` it is
a helper of returned, say) ends the server at once and keeps the
listener: what is in flight ends as below, and clients that connect wait
in the listener for another `http.serve` or a `tcp.accept`. A
`task.deadline` around `http.serve` does the same when its time is up,
also while the server drains: a drain that must not last longer than so
long is a deadline around `http.serve`.

```silt
import bytes
import http
import task
import tcp
import url

fn hello(_req: http.Request) -> http.Response {
  http.Response { status: 200, body: bytes.from_string("hi"), headers: #{} }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {
    panic("cannot listen")
  }
  let port = tcp.local_port(listener)
  when let Ok(home) = url.parse("http://127.0.0.1:{port}/") else {
    panic("no URL")
  }
  task.join(
    [
      { -> http.serve(listener, hello) },
      { ->
        match http.get(home) {
          Ok(r) -> println(r.status) -- 200
          Err(e) -> println("{e}")
        }
        -- the server answers what is in flight, and http.serve returns
        tcp.close_listener(listener)
      },
    ],
  )
  match http.get(home) {
    Ok(_) -> println("still served")
    Err(_) -> println("refused") -- refused
  }
}
```

When the task that serves is stopped (or its deadline passes):

- Its accept is given up. A client that connects afterwards waits in the
  listener for whoever accepts next (or is refused, if the listener was
  closed).
- A request whose handler has not returned is answered `503`, and its
  connection is closed. A connection between two requests is closed. A
  response that is being sent at that moment is cut off: the client
  gets less than its `Content-Length` says.
- After a stop, the listener is the program's again as soon as the call
  that stopped the server has returned: `tcp.accept` on it gets the next client,
  and another `http.serve` serves it. A closed listener serves nothing:
  `http.serve` on it returns `()` at once.

An accept that fails (the process has no file descriptor left) is
written to stderr once, and the server tries again after 100 ms.

Use pattern matching on `(req.method, url.segments(req.url))` for routing:

```silt
import bytes
import http
import json
import tcp
import url

type User {
  id: Int,
  name: String,
}

-- A body is bytes: this server answers with text, and says so.
fn reply(status: Int, text: String) -> http.Response {
  http.Response {
    status,
    body: bytes.from_string(text),
    headers: #{ "Content-Type": "text/plain; charset=UTF-8" },
  }
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:8080") else {
    panic("cannot listen on port 8080")
  }
  println("Listening on :{tcp.local_port(listener)}")

  http.serve(listener) { req ->
    match (req.method, url.segments(req.url)) {
      (http.GET, []) -> reply(200, "Hello!")

      (http.GET, ["users", id]) -> reply(200, "User {id}")

      (http.POST, ["users"]) -> match bytes.to_string(req.body) {
        Ok(text) -> match json.parse(text, User) {
          Ok(user) -> http.Response {
            status: 201,
            body: bytes.from_string(json.stringify(user)),
            headers: #{ "Content-Type": "application/json" },
          }
          Err(e) -> reply(400, "{e}")
        }
        Err(e) -> reply(400, "{e}")
      }

      _ -> reply(404, "Not found")
    }
  }
}
```
