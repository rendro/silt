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
| `serve` | `(TcpListener, Fn(Request) -> Response) -> ()` | Serve HTTP on a listener made with `tcp.listen`, a task per request |
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

Each incoming request is handled by a task of its own, so multiple
requests are processed concurrently, and a handler that waits (for a
channel, a timer, another request: a long poll) holds no thread while it
does. At most 128 handlers run at a time; a request beyond that is
answered `503` at once. If a handler function errors, the server returns
a 500 response without crashing. The handler receives a `Request` and
must return a `Response`.

`http.serve` returns only when its server ends. To run a server beside
other work, call it in a task; `task.cancel` of that task ends the
server (see [When a program ends](../concurrency.md#when-a-program-ends)).

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

Unsupported HTTP methods (e.g. TRACE) receive an automatic 405 response.


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

fn main() {
  http.parse_query("name=alice&tag=dev&tag=admin")
  -- #{"name": ["alice"], "tag": ["dev", "admin"]}

  http.parse_query("?q=hello%20world")
  -- #{"q": ["hello world"]}

  http.parse_query("")
  -- #{}
}
```

Like `http.segments`, this function has no network dependencies and works
with `--no-default-features`. Pair it with `req.query` in an `http.serve`
handler to route on query parameters.
