use silt::compiler::Compiler;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::value::Value;
use silt::vm::Vm;
use std::sync::Arc;

fn run(input: &str) -> Value {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = compiler.compile_program(&program).expect("compile error");
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    vm.run(script).expect("runtime error")
}

fn run_err(input: &str) -> String {
    let tokens = Lexer::new(input).tokenize().expect("lexer error");
    let mut program = Parser::new(tokens).parse_program().expect("parse error");
    let _ = silt::typechecker::check(&mut program);
    let mut compiler = Compiler::new();
    let functions = match compiler.compile_program(&program) {
        Ok(f) => f,
        Err(e) => return e.message,
    };
    let script = Arc::new(functions.into_iter().next().unwrap());
    let mut vm = Vm::new();
    let err = vm.run(script).expect_err("expected runtime error");
    format!("{err}")
}

// ── Spread in list literals ─────────────────────────────────────────

#[test]
fn test_list_spread_non_list_error() {
    let err = run_err(
        r#"
fn main() {
  let x = 42
  [1, ..x]
}
    "#,
    );
    // Production message from src/vm/run.rs ListConcat error.
    assert!(
        err.contains("ListConcat: right operand is not a list or range"),
        "expected list spread error, got: {err}"
    );
}

// ── Typed AST verification ──────────────────────────────────────────

#[test]
fn test_typed_ast_int_literal() {
    let input = r#"
fn main() {
  42
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "body should be typed");
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_string_expr() {
    let input = r#"
fn main() {
  "hello"
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert_eq!(f.body.ty, Some(silt::types::Type::String));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_list() {
    let input = r#"
fn main() {
  [1, 2, 3]
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "body should be typed");
        assert_eq!(
            f.body.ty,
            Some(silt::types::Type::List(Box::new(silt::types::Type::Int)))
        );
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_binary_expr() {
    let input = r#"
fn main() {
  let x = 10
  x + 32
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    silt::typechecker::check(&mut program);

    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "main body should be typed");
        // Block containing `let x = 10; x + 32` should resolve to Int
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

#[test]
fn test_typed_ast_function_return_type() {
    // check_fn_body now unifies body type with return type.
    // The function's own body should have a resolved type.
    let input = r#"
fn double(x) {
  x * 2
}

fn main() {
  double(21)
}
    "#;
    let tokens = silt::lexer::Lexer::new(input).tokenize().expect("lex");
    let mut program = silt::parser::Parser::new(tokens)
        .parse_program()
        .expect("parse");
    silt::typechecker::check(&mut program);

    // double's body (x * 2) should resolve to Int
    if let silt::ast::Decl::Fn(f) = &program.decls[0] {
        assert!(f.body.ty.is_some(), "double body should be typed");
        assert_eq!(f.body.ty, Some(silt::types::Type::Int));
    } else {
        panic!("expected fn decl");
    }
}

// ── Mixed int/float arithmetic ──────────────────────────────────────

#[test]
fn test_mixed_int_float_add() {
    let err = run_err(
        r#"
fn main() { 1 + 2.5 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_float_int_sub() {
    let err = run_err(
        r#"
fn main() { 10.0 - 3 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_int_float_div() {
    let err = run_err(
        r#"
fn main() { 7 / 2.0 }
    "#,
    );
    assert!(err.contains("cannot mix Int and Float"), "got: {err}");
}

#[test]
fn test_mixed_arithmetic_in_pipeline() {
    let result = run(r#"
import float
import int
fn main() {
  let total = 100
  let ratio = int.to_float(total) / 3.0
  float.to_string(ratio, 2)
}
    "#);
    assert_eq!(result, Value::String("33.33".into()));
}

// ── Cross-type comparison errors ────────────────────────────────────

#[test]
fn test_cross_type_eq_is_error() {
    let err = run_err(
        r#"
fn main() { 5 == "hello" }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_cross_type_lt_is_error() {
    let err = run_err(
        r#"
fn main() { 3 < true }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

#[test]
fn test_cross_type_int_float_eq_is_error() {
    let err = run_err(
        r#"
fn main() { 3 == 3.0 }
    "#,
    );
    assert!(err.contains("unsupported operation"), "got: {err}");
}

// ════════════════════════════════════════════════════════════════════
// HTTP Module Tests
// ════════════════════════════════════════════════════════════════════

#[test]
fn test_http_segments_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.segments("/a", "/b")
}
    "#,
    );
    assert!(err.contains("http.segments takes 1 argument"), "got: {err}");
}

#[test]
fn test_http_segments_wrong_type() {
    let err = run_err(
        r#"
import http
fn main() {
  http.segments(42)
}
    "#,
    );
    assert!(
        err.contains("http.segments requires String, got"),
        "got: {err}"
    );
}

// ── http.parse_query ────────────────────────────────────────────────

#[test]
fn test_http_parse_query_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.parse_query("a=1", "b=2")
}
    "#,
    );
    assert!(
        err.contains("http.parse_query takes 1 argument"),
        "got: {err}"
    );
}

#[test]
fn test_http_parse_query_wrong_type() {
    let err = run_err(
        r#"
import http
fn main() {
  http.parse_query(42)
}
    "#,
    );
    assert!(
        err.contains("http.parse_query requires String, got"),
        "got: {err}"
    );
}

#[test]
fn test_http_get_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.get("http://example.com", "extra")
}
    "#,
    );
    assert!(err.contains("http.get takes 1 argument"), "got: {err}");
}

#[test]
fn test_http_get_wrong_type() {
    let err = run_err(
        r#"
import http
fn main() {
  http.get(42)
}
    "#,
    );
    assert!(err.contains("http.get requires String, got"), "got: {err}");
}

#[test]
fn test_http_request_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, "http://example.com")
}
    "#,
    );
    assert!(err.contains("http.request takes 4 arguments"), "got: {err}");
}

#[test]
fn test_http_request_non_variant_method() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request("GET", "http://example.com", "", #{})
}
    "#,
    );
    assert!(
        err.contains("http.request requires Method, got"),
        "got: {err}"
    );
}

#[test]
fn test_http_request_non_string_url() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, 42, "", #{})
}
    "#,
    );
    assert!(
        err.contains("http.request requires String, got"),
        "got: {err}"
    );
}

#[test]
fn test_http_request_non_string_body() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(POST, "http://example.com", 42, #{})
}
    "#,
    );
    assert!(
        err.contains("http.request requires String, got"),
        "got: {err}"
    );
}

#[test]
fn test_http_request_non_map_headers() {
    let err = run_err(
        r#"
import http
fn main() {
  http.request(GET, "http://example.com", "", "bad")
}
    "#,
    );
    assert!(err.contains("http.request requires Map, got"), "got: {err}");
}

#[test]
fn test_http_serve_wrong_arg_count() {
    let err = run_err(
        r#"
import http
fn main() {
  http.serve(8080)
}
    "#,
    );
    assert!(err.contains("http.serve takes 2 arguments"), "got: {err}");
}

#[test]
fn test_http_serve_non_int_port() {
    let err = run_err(
        r#"
import http
fn main() {
  http.serve("8080", { req -> Response { status: 200, body: "", headers: #{} } })
}
    "#,
    );
    assert!(err.contains("http.serve requires Int, got"), "got: {err}");
}

#[test]
fn test_http_unknown_function() {
    let err = run_err(
        r#"
import http
fn main() {
  http.nonexistent()
}
    "#,
    );
    assert!(err.contains("unknown http function"), "got: {err}");
}

// ── http.serve concurrency ──────────────────────────────────────

#[test]
fn test_http_serve_non_blocking_in_task() {
    // Spawn http.serve in a task; verify other tasks can still run.
    // The accept loop runs on a dedicated OS thread and the silt task
    // yields via BlockReason::Join, so scheduler workers stay free.
    //
    // Port is OS-assigned (ephemeral) rather than hardcoded, so
    // parallel test runs / a stuck previous process can't cause
    // `AddrInUse` flakes. Same pattern as the sibling
    // `test_http_serve_basic_get_response` (see `find_free_port`).
    let port = find_free_port();
    let input = format!(
        r#"
import http
import task
import channel

fn main() {{
  let done = channel.new(1)
  let server = task.spawn({{ ->
    http.serve({port}, {{ req ->
      Response {{ status: 200, body: "ok", headers: #{{}} }}
    }})
  }})
  let worker = task.spawn({{ ->
    channel.send(done, "ready")
  }})
  let result = channel.receive(done)
  match result {{
    Message(v) -> v
    _ -> "failed"
  }}
}}
"#
    );
    let result = run(&input);
    assert_eq!(result, Value::String("ready".into()));
}

#[test]
fn test_http_serve_concurrent_requests() {
    // Start a server on the main thread (it blocks), send concurrent
    // HTTP requests from Rust threads, and verify all get correct
    // responses — proving per-request concurrency.
    //
    // Port is OS-assigned (ephemeral) rather than hardcoded 19081, so
    // parallel test runs / a stuck previous process can't cause
    // `AddrInUse` flakes. The brief bind→connect race is handled by
    // `wait_for_port` — same pattern as the sibling
    // `test_http_serve_basic_get_response`.
    use std::thread;

    let port = find_free_port();

    // Run the silt server in a background thread (http.serve blocks the main thread).
    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    // Wait for the server to bind and start accepting
    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    // Send 5 concurrent requests
    let mut request_handles = Vec::new();
    for i in 0..5 {
        request_handles.push(thread::spawn(move || {
            let url = format!("http://127.0.0.1:{port}/path{i}");
            match ureq::get(&url).call() {
                Ok(mut resp) => resp.body_mut().read_to_string().unwrap_or_default(),
                Err(e) => format!("error: {e}"),
            }
        }));
    }

    for (i, h) in request_handles.into_iter().enumerate() {
        let body = h.join().unwrap();
        assert_eq!(body, format!("/path{i}"), "request {i} got wrong response");
    }
}

// ── http.serve functional tests ─────────────────────────────────

/// Find a free ephemeral port by binding to port 0, recording the
/// assigned port, then dropping the listener so `http.serve` can bind it.
fn find_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Spin-wait for a TCP port to become connectable, with a timeout.
/// Returns `true` if the port is ready, `false` if timed out.
fn wait_for_port(port: u16, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

#[test]
fn test_http_serve_basic_get_response() {
    // Start a server that echoes a fixed body, make a GET request, verify
    // the response body matches.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: "hello from silt", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "hello from silt");
}

#[test]
fn test_http_serve_returns_custom_status_code() {
    // Handler returns a 404 status — verify the client sees it.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 404, body: "not found", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/missing"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 404);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "not found");
}

#[test]
fn test_http_serve_echoes_request_path() {
    // Handler echoes back the request path in the body.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // Test various paths
    for path in &["/", "/api/v1/users", "/hello/world"] {
        let mut resp = agent
            .get(&format!("http://127.0.0.1:{port}{path}"))
            .call()
            .expect("GET request failed");

        assert_eq!(resp.status(), 200);
        let body = resp.body_mut().read_to_string().unwrap();
        assert_eq!(body, *path, "path mismatch for {path}");
    }
}

#[test]
fn test_http_serve_echoes_query_string() {
    // Handler echoes back the query string from the request.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: req.query, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/search?q=silt&page=1"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "q=silt&page=1");
}

#[test]
fn test_http_serve_reads_request_body() {
    // POST a body to the server and verify the handler receives it.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: req.body, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .post(&format!("http://127.0.0.1:{port}/echo"))
        .send("request body content")
        .expect("POST request failed");

    assert_eq!(resp.status(), 200);
    let body = resp.body_mut().read_to_string().unwrap();
    assert_eq!(body, "request body content");
}

#[test]
fn test_http_serve_reads_request_method() {
    // Handler pattern-matches on the request method and returns its name.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    let method_name = match req.method {{
      GET -> "got-get"
      POST -> "got-post"
      PUT -> "got-put"
      DELETE -> "got-delete"
      _ -> "got-other"
    }}
    Response {{ status: 200, body: method_name, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // Test GET
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-get");

    // Test POST
    let mut resp = agent
        .post(&format!("http://127.0.0.1:{port}/"))
        .send_empty()
        .expect("POST failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-post");

    // Test PUT
    let mut resp = agent
        .put(&format!("http://127.0.0.1:{port}/"))
        .send_empty()
        .expect("PUT failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-put");

    // Test DELETE
    let mut resp = agent
        .delete(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("DELETE failed");
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "got-delete");
}

#[test]
fn test_http_serve_sets_response_headers() {
    // Handler sets custom response headers — verify the client sees them.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{
      status: 200,
      body: "ok",
      headers: #{{ "X-Custom": "silt-value", "X-Another": "42" }}
    }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent
        .get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("GET request failed");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("X-Custom").and_then(|v| v.to_str().ok()),
        Some("silt-value"),
        "missing or wrong X-Custom header"
    );
    assert_eq!(
        resp.headers()
            .get("X-Another")
            .and_then(|v| v.to_str().ok()),
        Some("42"),
        "missing or wrong X-Another header"
    );
}

#[test]
fn test_http_serve_routing_by_path() {
    // Handler routes requests based on path, returning different responses.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    match req.path {{
      "/health" -> Response {{ status: 200, body: "ok", headers: #{{}} }}
      "/greet" -> Response {{ status: 200, body: "hello!", headers: #{{}} }}
      _ -> Response {{ status: 404, body: "not found", headers: #{{}} }}
    }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();

    // /health -> 200 "ok"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .expect("GET /health failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "ok");

    // /greet -> 200 "hello!"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/greet"))
        .call()
        .expect("GET /greet failed");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "hello!");

    // /unknown -> 404 "not found"
    let mut resp = agent
        .get(&format!("http://127.0.0.1:{port}/unknown"))
        .call()
        .expect("GET /unknown failed");
    assert_eq!(resp.status(), 404);
    assert_eq!(resp.body_mut().read_to_string().unwrap(), "not found");
}

#[test]
fn test_http_serve_concurrent_requests_stress() {
    // Stress test: 20 concurrent requests, each with a unique path.
    // Verifies the server handles high concurrency correctly.
    use std::thread;

    let port = find_free_port();

    thread::spawn(move || {
        let input = format!(
            r#"
import http

fn main() {{
  http.serve({port}, {{ req ->
    Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    assert!(
        wait_for_port(port, std::time::Duration::from_secs(3)),
        "server did not start"
    );

    let count = 20;
    let mut handles = Vec::new();
    for i in 0..count {
        handles.push(thread::spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let url = format!("http://127.0.0.1:{port}/stress/{i}");
            let mut resp = agent.get(&url).call().expect("request failed");
            let body = resp.body_mut().read_to_string().unwrap();
            (i, resp.status(), body)
        }));
    }

    for h in handles {
        let (i, status, body) = h.join().unwrap();
        assert_eq!(status, 200, "request {i} got status {status}");
        assert_eq!(body, format!("/stress/{i}"), "request {i} got wrong body");
    }
}

#[test]
fn test_http_serve_from_task_with_silt_client() {
    // Start the server in a spawned task, then use http.get() from another
    // task to make a request to it — fully within Silt's runtime.
    // The client retries in a functional loop until the server is ready.
    use std::thread;

    let port = find_free_port();

    // Run the entire program in a background Rust thread to prevent
    // hanging the test runner if something goes wrong.
    let handle = thread::spawn(move || {
        let input = format!(
            r#"
import http
import task
import channel

fn main() {{
  let result_ch = channel.new(1)

  -- Start the server in a task
  let server = task.spawn({{ ->
    http.serve({port}, {{ req ->
      Response {{ status: 200, body: "silt-response", headers: #{{}} }}
    }})
  }})

  -- Make a request from another task, retrying until the server is up
  let client = task.spawn({{ ->
    let body = loop attempts = 0 {{
      match attempts > 100 {{
        true -> "gave up"
        _ -> match http.get("http://127.0.0.1:{port}/test") {{
          Ok(resp) -> resp.body
          Err(_) -> loop(attempts + 1)
        }}
      }}
    }}
    channel.send(result_ch, body)
  }})

  let Message(body) = channel.receive(result_ch)
  body
}}
"#
        );
        run(&input)
    });

    let result = handle.join().expect("silt program panicked");
    assert_eq!(result, Value::String("silt-response".into()));
}

// ── Lambda as immediately-invoked expression ────────────────────────

#[test]
fn test_lambda_iife() {
    let result = run(r#"
fn main() {
  let result = ({ x, y -> x + y })(3, 4)
  result
}
    "#);
    assert_eq!(result, Value::Int(7));
}

// ── Tuple index access ──────────────────────────────────────────────

#[test]
fn test_tuple_numeric_field_access() {
    let result = run(r#"
fn main() {
  let t = (10, 20, 30)
  t.0 + t.1 + t.2
}
    "#);
    assert_eq!(result, Value::Int(60));
}

// ── Float constants and math ────────────────────────────────────────

#[test]
fn test_float_named_constants() {
    // Float constants
    assert_eq!(
        run(r#"
import float
fn main() { float.max_value }
    "#),
        Value::Float(f64::MAX)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.min_value }
    "#),
        Value::Float(f64::MIN)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.epsilon }
    "#),
        Value::Float(f64::EPSILON)
    );
    assert_eq!(
        run(r#"
import float
fn main() { float.min_positive }
    "#),
        Value::Float(f64::MIN_POSITIVE)
    );
}

#[test]
fn test_math_sin_cos() {
    assert_eq!(
        run(r#"
import math
fn main() { math.sin(0.0) }
    "#),
        Value::Float(0.0)
    );
    assert_eq!(
        run(r#"
import math
fn main() { math.cos(0.0) }
    "#),
        Value::Float(1.0)
    );
}

#[test]
fn test_math_atan() {
    assert_eq!(
        run(r#"
import math
fn main() { math.atan(0.0) }
    "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_math_atan2() {
    assert_eq!(
        run(r#"
import math
fn main() { math.atan2(0.0, 1.0) }
    "#),
        Value::Float(0.0)
    );
}

#[test]
fn test_math_tan() {
    assert_eq!(
        run(r#"
import math
fn main() { math.tan(0.0) }
    "#),
        Value::Float(0.0)
    );
}
