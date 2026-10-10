---
title: "http"
section: "Standard Library"
order: 14
---

# http

HTTP client and server. Included by default (the server needs [tcp](tcp.md), which the `http` feature brings with it). Exclude with `--no-default-features` for WASM or minimal builds (networking functions will return a runtime error, but `http.segments` still works).

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
  path: String,
  query: String,
  headers: Map(String, String),
  body: String,
}

type Response {
  status: Int,
  body: String,
  headers: Map(String, String),
}
```

`Method` and its variants are members of `http`: `http.Method`, `http.GET`, `http.POST`, etc.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `get` | `(String) -> Result(Response, HttpError)` | HTTP GET request |
| `request` | `(Method, String, String, Map(String, String)) -> Result(Response, HttpError)` | HTTP request with method, URL, body, headers |
| `serve` | `(TcpListener, Fn(Request) -> Response) -> ()` | Serve HTTP on a listener made with `tcp.listen`, a task per connection |
| `segments` | `(String) -> List(String)` | Split URL path into segments |
| `parse_query` | `(String) -> Map(String, List(String))` | Parse a URL query string into a multi-value map |

## Errors

`http.get` and `http.request` return `Result(Response, HttpError)`. Note
that a 4xx or 5xx HTTP response is an `Ok(Response)` — only failures
*before* a response lands (DNS, connection, TLS, protocol) become `Err`.
Servers that explicitly want to short-circuit on a non-2xx code can
construct `HttpStatusCode(status, body)` themselves; the stdlib does
not do that conversion for you. `HttpError` implements the built-in
`Error` trait, so `e.message()` always yields a rendered string.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `HttpConnect(msg)` | `String` | TCP / DNS connect failure |
| `HttpTls(msg)` | `String` | TLS handshake / cert failure |
| `HttpTimeout` | — | request exceeded its deadline |
| `HttpInvalidUrl(url)` | `String` | URL did not parse |
| `HttpInvalidResponse(msg)` | `String` | response violated protocol |
| `HttpClosedEarly` | — | peer closed before response completed |
| `HttpStatusCode(status, body)` | `Int, String` | user-constructed for non-success codes |
| `HttpUnknown(msg)` | `String` | unclassified transport error |


## `http.get`

```
http.get(url: String) -> Result(Response, HttpError)
```

Makes an HTTP GET request. Returns `Ok(Response)` for any successful
connection (including 4xx/5xx status codes). Returns `Err(HttpError)`
for network errors (DNS failure, connection refused, timeout, TLS).

When called from a spawned task, `http.get` transparently yields to the
scheduler while the request is in flight. No API change is needed -- the
call site looks the same.

```silt
import http
import string

fn main() {
  match http.get("https://api.github.com/users/torvalds") {
    Ok(resp) -> println("Status: {resp.status}, body length: {string.length(resp.body)}")
    Err(http.HttpTimeout) -> println("timed out; retry later")
    Err(e) -> println("Network error: {e.message()}")
  }
}
```

Compose with `json.parse` and `?` for typed API responses. Since
`http.get` and `json.parse` return different error types, wrap each in
a local enum using `result.map_err` with a variant constructor as a
first-class `Fn`:

```silt
import http
import json
import result

type User {
  name: String,
  id: Int,
}

type FetchError {
  Network(http.HttpError),
  Parse(json.JsonError),
}

fn fetch_user(name: String) -> Result(User, FetchError) {
  let resp = http.get("https://api.example.com/users/{name}") |> result.map_err(Network)?
  json.parse(resp.body, User) |> result.map_err(Parse)
}
```


## `http.request`

```
http.request(method: Method, url: String, body: String, headers: Map(String, String)) -> Result(Response, HttpError)
```

Makes an HTTP request with full control over method, body, and headers. Use this for POST, PUT, DELETE, or any request that needs custom headers.

Like `http.get`, this transparently yields to the scheduler when called from
a spawned task.

```silt
-- POST with JSON body
let resp = http.request(
  http.POST,
  "https://api.example.com/users",
  json.stringify(#{ "name": "Alice" }),
  #{ "Content-Type": "application/json", "Authorization": "Bearer tok123" },
)?

-- DELETE
let resp = http.request(http.DELETE, "https://api.example.com/users/42", "", #{})?

-- GET with custom headers
let resp = http.request(
  http.GET,
  "https://api.example.com/data",
  "",
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

`http.serve` does not return: it serves until its task ends. To run a
server beside other work, call it in a task; `task.cancel` of that task
ends the server (see [When a program ends](../concurrency.md#when-a-program-ends)).

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
| The handler fails, or returns a status that is none | `500`. The failure is written to stderr and nothing of it is sent. The connection stays usable. |
| 128 handlers are being called already | `503` at once. That includes a request that would have released the ones that wait: a server whose handlers wait for each other needs a second server, or a timeout in the handler. |
| The method is none of `Method`'s (e.g. `TRACE`) | `405`, with an `Allow` header that names the seven |
| The method is `HEAD` | the handler is called; its response is sent without the body, with the length the body has |

**A client that leaves is not noticed while its handler runs.** The
server looks at a connection when it reads from it or writes to it. If a
client closes its connection while its handler waits (a long poll), the
handler goes on waiting: it keeps its place among the 128, and the
connection its descriptor, until the handler returns and the response
cannot be sent. Clients that come and go without waiting for their
answers can so fill every place, and every later request gets `503`. A
handler that waits needs a limit of its own, for example
`channel.recv_timeout` instead of `channel.receive`.

### What the server reads and sends

The server speaks HTTP/1.1, and HTTP/1.0 to a client that does (the
connection is then closed after the response unless the request says
`Connection: keep-alive`). A request body is given by `Content-Length`
or sent in chunks (`Transfer-Encoding: chunked`; trailers are read and
dropped); `req.body` is the body, decoded. A client that sends `Expect:
100-continue` is told to go on before its body is read. `Connection:
close` closes the connection after the response.

A response is sent with `Content-Length`, `Date` and, where the
connection ends with it, `Connection: close`. Those three are the
server's: headers of these names in `Response.headers` are left out, and
so is a header whose name or value could not stand in one header line.
`Content-Type` is `text/plain; charset=UTF-8` unless the handler sets
one. There is no `Server` header.

### Limits

| Limit | | Beyond it |
|---|---|---|
| The head of a request: request line and headers | 64 KiB | `431`, connection closed |
| Headers of a request | 100 | `431`, connection closed |
| The body of a request | 10 MiB | `413`, connection closed |
| The bodies, together, of the requests that no handler has yet: those being read (a body counts from when its length is known) and those read and about to be handed over | 256 MiB | `503`, connection closed: the request whose body would not fit is refused before its body is read. With every handler holding a request of the largest size besides, the server holds 128 x 10 MiB more: about 1.5 GiB of bodies is the worst case. |
| A method | 32 bytes | `400`, connection closed |
| A line of a chunked body: a chunk's size and its extensions | 4 KiB | `400`, connection closed |
| What of a chunked body is not the body: chunk sizes, extensions, line ends | 1 MiB | `400`, connection closed. (A 10 MiB body in chunks of 100 bytes is within it; a body cut into single bytes is not.) |
| The trailers of a chunked body | 64 KiB | `431`, connection closed |
| Handlers being called | 128 | `503` |
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
on it is not served.

An `Expect` header that asks for anything but `100-continue` is answered
`417`, and the connection closed.

### What the handler gets

- `req.headers` has the names as the client sent them: a lookup is
  case-sensitive, and `Content-Type` and `content-type` are two keys. Of
  a header sent twice under the same spelling the last one stays.
- `req.body` is a `String`: bytes of a body that are not UTF-8 are
  replaced (U+FFFD), so a binary upload does not arrive intact.
- `req.path` and `req.query` are the request target as sent, cut at the
  first `?`: nothing is decoded or normalised. A target in absolute form
  (`GET http://other.example/x HTTP/1.1`) is handed over whole, and a
  `#fragment` stays where it is.
- A handler that cancels its own server still has its response sent.
- If the program fails while requests are in flight, it ends as a
  process: their connections are closed without an answer.

These times are the server's own, and the only ones that bound its
waits. A `task.deadline` around `http.serve` does not end the server,
nor a connection's wait for a request, a body or a client; neither does
`SILT_IO_TIMEOUT`. Inside a handler both work as in any task: the
handler's own I/O ends at `SILT_IO_TIMEOUT`, and a `task.deadline` that
the handler sets bounds what it calls. A deadline set around
`http.serve` is not handed on to the handlers.

Every connection that waits for a request has one I/O operation in
flight, of the 4,096 a program can have (see
[concurrency](../concurrency.md)).

### The listener while it is served

While `http.serve` serves a listener, the listener is the server's alone:

- `tcp.accept`, `tcp.accept_tls` and `tcp.accept_tls_mtls` on it return
  `Err(TcpUnknown("the listener is served by http.serve"))` at once. An
  accept that was already waiting on the listener when the server started
  returns the same error at that moment: the server has every client.
- A second `http.serve` on it is a runtime error.

When the task that serves has been cancelled, the server ends:

- Its accept is given up. A client that connects afterwards waits in the
  listener for whoever accepts next.
- A request whose handler has not returned is answered `503`, and its
  connection is closed. A connection between two requests is closed. A
  response that is being sent at that moment is cut off: the client
  gets less than its `Content-Length` says.
- The listener is the program's again as soon as `task.cancel` has
  returned: `tcp.accept` on it gets the next client, and another
  `http.serve` serves it.

An accept that fails (the process has no file descriptor left) is
written to stderr once, and the server tries again after 100 ms.

Use pattern matching on `(req.method, segments)` for routing:

```silt
import http
import json
import tcp

type User {
  id: Int,
  name: String,
}

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:8080") else {
    panic("cannot listen on port 8080")
  }
  println("Listening on :{tcp.local_port(listener)}")

  http.serve(listener) { req ->
    match (req.method, http.segments(req.path)) {
      (http.GET, []) -> http.Response { status: 200, body: "Hello!", headers: #{} }

      (http.GET, ["users", id]) -> http.Response { status: 200, body: "User {id}", headers: #{} }

      (http.POST, ["users"]) -> match json.parse(req.body, User) {
        Ok(user) -> http.Response {
          status: 201,
          body: json.stringify(user),
          headers: #{ "Content-Type": "application/json" },
        }
        Err(e) -> http.Response { status: 400, body: e.message(), headers: #{} }
      }

      _ -> http.Response { status: 404, body: "Not found", headers: #{} }
    }
  }
}
```


## `http.segments`

```
http.segments(path: String) -> List(String)
```

Splits a URL path into non-empty segments. Useful for pattern-matched routing.

```silt
http.segments("/api/users/42") -- ["api", "users", "42"]
http.segments("/") -- []
http.segments("//foo//bar/") -- ["foo", "bar"]
```

This function has no dependencies and works even with `--no-default-features`.


## `http.parse_query`

```
http.parse_query(query: String) -> Map(String, List(String))
```

Parses a URL query string into a map from key to a list of values. Repeated
keys accumulate into the same list in the order they appear, so a query like
`tag=a&tag=b` parses as `#{"tag": ["a", "b"]}`.

- A leading `?` is accepted and ignored.
- Percent escapes (`%HH`) in both keys and values are decoded. Invalid or
  truncated escapes cause a runtime error.
- Following the `application/x-www-form-urlencoded` convention, `+` decodes
  to a space in values.
- A key with no `=` (e.g. `flag&other=x`) is treated as having an empty
  string value: `#{"flag": [""], "other": ["x"]}`.
- Empty segments from leading, doubled, or trailing `&` are silently skipped.
- An empty input (or a bare `?`) returns the empty map.

```silt
import http
import io

fn main() {
  println(io.inspect(http.parse_query("name=alice&tag=dev&tag=admin")))
  -- #{"name": ["alice"], "tag": ["dev", "admin"]}

  println(io.inspect(http.parse_query("?q=hello%20world")))
  -- #{"q": ["hello world"]}

  println(io.inspect(http.parse_query("")))
  -- #{}
}
```

Like `http.segments`, this function has no network dependencies and works
with `--no-default-features`. Pair it with `req.query` in an `http.serve`
handler to route on query parameters.
