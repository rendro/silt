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
| `get` | `(String) -> Result(Response, String)` | HTTP GET request |
| `request` | `(Method, String, String, Map(String, String)) -> Result(Response, String)` | HTTP request with method, URL, body, headers |
| `serve` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server |
| `segments` | `(String) -> List(String)` | Split URL path into segments |


## `http.get`

```
http.get(url: String) -> Result(Response, String)
```

Makes an HTTP GET request. Returns `Ok(Response)` for any successful connection (including 4xx/5xx status codes). Returns `Err(message)` for network errors (DNS failure, connection refused, timeout).

When called from a spawned task, `http.get` transparently yields to the
scheduler while the request is in flight. No API change is needed -- the
call site looks the same.

```silt
fn main() {
  match http.get("https://api.github.com/users/torvalds") {
    Ok(resp) -> println("Status: {resp.status}, body length: {string.length(resp.body)}")
    Err(e) -> println("Network error: {e}")
  }
}
```

Compose with `json.parse` and `?` for typed API responses:

```silt
type User { name: String, id: Int }

fn fetch_user(name) {
  let resp = http.get("https://api.example.com/users/{name}")?
  json.parse(User, resp.body)
}
```


## `http.request`

```
http.request(method: Method, url: String, body: String, headers: Map(String, String)) -> Result(Response, String)
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

Starts an HTTP server on the given port. Each incoming request is handled on
its own thread with a fresh VM, so multiple requests are processed
concurrently. The accept loop runs on a dedicated OS thread and does not block
the scheduler. If a handler function errors, the server returns a 500 response
without crashing. The handler receives a `Request` and must return a
`Response`. The server runs forever (stop with Ctrl-C).

Use pattern matching on `(req.method, segments)` for routing:

```silt
fn main() {
  println("Listening on :8080")

  http.serve(8080, fn(req) {
    let parts = string.split(req.path, "/")
      |> list.filter { s -> !string.is_empty(s) }

    match (req.method, parts) {
      (GET, []) ->
        Response { status: 200, body: "Hello!", headers: #{} }

      (GET, ["users", id]) ->
        Response { status: 200, body: "User {id}", headers: #{} }

      (POST, ["users"]) ->
        match json.parse(User, req.body) {
          Ok(user) -> Response {
            status: 201,
            body: json.stringify(user),
            headers: #{"Content-Type": "application/json"},
          }
          Err(e) -> Response { status: 400, body: e, headers: #{} }
        }

      _ ->
        Response { status: 404, body: "Not found", headers: #{} }
    }
  })
}
```

Unsupported HTTP methods (e.g. TRACE) receive an automatic 405 response.


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
