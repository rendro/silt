---
title: "http"
section: "Standard Library"
order: 14
---

# http

HTTP client and server. Included by default. Exclude with `--no-default-features` for WASM or minimal builds (networking functions will return a runtime error, but `http.segments` still works).

## Types

```silt
type Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS }

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

`Method` variants are gated constructors -- using `GET`, `POST`, etc. requires `import http`.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `get` | `(String) -> Result(Response, HttpError)` | HTTP GET request |
| `request` | `(Method, String, String, Map(String, String)) -> Result(Response, HttpError)` | HTTP request with method, URL, body, headers |
| `serve` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server bound to `127.0.0.1` (loopback only) |
| `serve_all` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server bound to `0.0.0.0` (all interfaces) |
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
    Err(HttpTimeout) -> println("timed out; retry later")
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

type User { name: String, id: Int }

type FetchError {
  Network(HttpError),
  Parse(JsonError),
}

fn fetch_user(name: String) -> Result(User, FetchError) {
  let resp = http.get("https://api.example.com/users/{name}")
    |> result.map_err(Network)?
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
  POST,
  "https://api.example.com/users",
  json.stringify(#{"name": "Alice"}),
  #{"Content-Type": "application/json", "Authorization": "Bearer tok123"}
)?

-- DELETE
let resp = http.request(DELETE, "https://api.example.com/users/42", "", #{})?

-- GET with custom headers
let resp = http.request(GET, "https://api.example.com/data", "", #{"Accept": "text/plain"})?
```


## `http.serve`

```
http.serve(port: Int, handler: Fn(Request) -> Response) -> ()
```

Starts an HTTP server on the given port, **bound to `127.0.0.1` (loopback
only)**. This is the safe default: the listener is only reachable from the
same host, so a development server is not accidentally exposed to the
network. To accept connections from other machines, use
[`http.serve_all`](#httpserve_all).

Each incoming request is handled on its own thread with a fresh VM, so
multiple requests are processed concurrently. The accept loop runs on a
dedicated OS thread and does not block the scheduler. If a handler function
errors, the server returns a 500 response without crashing. The handler
receives a `Request` and must return a `Response`. The server runs forever
(stop with Ctrl-C).

Use pattern matching on `(req.method, segments)` for routing:

```silt
import http
import json

type User { id: Int, name: String }

fn main() {
  println("Listening on :8080")

  http.serve(8080, fn(req) {
    match (req.method, http.segments(req.path)) {
      (GET, []) ->
        Response { status: 200, body: "Hello!", headers: #{} }

      (GET, ["users", id]) ->
        Response { status: 200, body: "User {id}", headers: #{} }

      (POST, ["users"]) ->
        match json.parse(req.body, User) {
          Ok(user) -> Response {
            status: 201,
            body: json.stringify(user),
            headers: #{"Content-Type": "application/json"},
          }
          Err(e) -> Response { status: 400, body: e.message(), headers: #{} }
        }

      _ ->
        Response { status: 404, body: "Not found", headers: #{} }
    }
  })
}
```

Unsupported HTTP methods (e.g. TRACE) receive an automatic 405 response.


## `http.serve_all`

```
http.serve_all(port: Int, handler: Fn(Request) -> Response) -> ()
```

Identical to [`http.serve`](#httpserve) except the listener is bound to
`0.0.0.0`, so the server accepts connections from *any* network interface
(localhost, LAN, and public IPs if the host is routed).

**Security rationale.** The default `http.serve` binds to `127.0.0.1` so a
development server cannot be accidentally exposed to the network — a
common source of data leaks when a laptop joins an untrusted Wi-Fi, or a
container is run without explicit port firewalling. `http.serve_all` is
the explicit opt-in for the minority of cases where binding all interfaces
is actually what you want (deployment behind a reverse proxy, LAN-only
services, containers where loopback is bridged). The two variants
otherwise behave identically — same concurrency caps, same body-size
limits, same error handling.

```silt
import http

fn main() {
  -- Accept connections from anywhere. Make sure this is really what
  -- you want before shipping.
  http.serve_all(8080) { _req ->
    Response { status: 200, body: "Hello, world!", headers: #{} }
  }
}
```


## `http.segments`

```
http.segments(path: String) -> List(String)
```

Splits a URL path into non-empty segments. Useful for pattern-matched routing.

```silt
http.segments("/api/users/42")   -- ["api", "users", "42"]
http.segments("/")               -- []
http.segments("//foo//bar/")     -- ["foo", "bar"]
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
