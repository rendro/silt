use silt::value::Value;

use crate::port_file::PortFile;

fn run(input: &str) -> Value {
    silt::session::testing::run_str(input).unwrap_or_else(|e| panic!("{e}"))
}

// ── Typed AST verification ──────────────────────────────────────────

#[test]
fn test_typed_ast_int_literal() {
    let input = r#"
fn main() {
  42
}
    "#;
    let (program, _) = silt::session::testing::analyze_str(input);

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
    let (program, _) = silt::session::testing::analyze_str(input);

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
    let (program, _) = silt::session::testing::analyze_str(input);

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
    let (program, _) = silt::session::testing::analyze_str(input);

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
    let (program, _) = silt::session::testing::analyze_str(input);

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

// ════════════════════════════════════════════════════════════════════
// HTTP Module Tests
// ════════════════════════════════════════════════════════════════════

// ── http.parse_query ────────────────────────────────────────────────

// ── http.serve concurrency ──────────────────────────────────────

#[test]
fn test_http_serve_non_blocking_in_task() {
    // Spawn http.serve in a task; verify other tasks can still run.
    // The accept loop runs on a dedicated OS thread and the silt task
    // yields via BlockReason::Join, so scheduler workers stay free.
    //
    // The listener is on a port the system chooses.
    let input = r#"
import http
import task
import channel
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let done = channel.new(1)
  let server = task.spawn({ ->
    http.serve(listener, { req ->
      http.Response { status: 200, body: "ok", headers: #{} }
    })
  })
  let worker = task.spawn({ ->
    channel.send(done, "ready")
  })
  let result = channel.receive(done)
  match result {
    channel.Message(v) -> v
    _ -> "failed"
  }
}
"#;
    let result = run(input);
    assert_eq!(result, Value::String("ready".into()));
}

#[test]
fn test_http_serve_concurrent_requests() {
    // Start a server on the main thread (it blocks), send concurrent
    // HTTP requests from Rust threads, and verify all get correct
    // responses — proving per-request concurrency.
    //
    // The program listens on a port the system chooses and writes it to
    // a file the test waits for (`PortFile`).
    use std::thread;

    let port_file = PortFile::new();
    let port_path = port_file.path();

    // Run the silt server in a background thread (http.serve blocks the main thread).
    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    // Wait for the server to bind and start accepting
    let port = port_file.wait();

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

#[test]
fn test_http_serve_basic_get_response() {
    // Start a server that echoes a fixed body, make a GET request, verify
    // the response body matches.
    use std::thread;

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: "hello from silt", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 404, body: "not found", headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: req.query, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: req.body, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    let method_name = match req.method {{
      http.GET -> "got-get"
      http.POST -> "got-post"
      http.PUT -> "got-put"
      http.DELETE -> "got-delete"
      _ -> "got-other"
    }}
    http.Response {{ status: 200, body: method_name, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{
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

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    match req.path {{
      "/health" -> http.Response {{ status: 200, body: "ok", headers: #{{}} }}
      "/greet" -> http.Response {{ status: 200, body: "hello!", headers: #{{}} }}
      _ -> http.Response {{ status: 404, body: "not found", headers: #{{}} }}
    }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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

    let port_file = PortFile::new();
    let port_path = port_file.path();

    thread::spawn(move || {
        let input = format!(
            r#"
import http
import io
import tcp

-- Listen on a port the system chooses, and tell the test which.
fn bound(port_file) {{
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else {{ panic("cannot listen") }}
  when let Ok(_) = io.write_file(port_file, "{{tcp.local_port(listener)}}\n") else {{
    panic("cannot write the port")
  }}
  listener
}}

fn main() {{
  http.serve(bound("{port_path}"), {{ req ->
    http.Response {{ status: 200, body: req.path, headers: #{{}} }}
  }})
}}
"#
        );
        run(&input);
    });

    let port = port_file.wait();

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
    // task to make a request to it — fully within Silt's runtime. The
    // listener is bound before either task starts, on a port the system
    // chooses, so the client's first request is queued for the server.
    use std::thread;

    // Run the entire program in a background Rust thread to prevent
    // hanging the test runner if something goes wrong.
    let handle = thread::spawn(move || {
        let input = r#"
import http
import task
import channel
import tcp

fn main() {
  when let Ok(listener) = tcp.listen("127.0.0.1:0") else { panic("cannot listen") }
  let port = tcp.local_port(listener)
  let result_ch = channel.new(1)

  -- Start the server in a task
  let server = task.spawn({ ->
    http.serve(listener, { req ->
      http.Response { status: 200, body: "silt-response", headers: #{} }
    })
  })

  -- Make a request from another task
  let client = task.spawn({ ->
    let body = match http.get("http://127.0.0.1:{port}/test") {
      Ok(resp) -> resp.body
      Err(e) -> "the request failed: {e.message()}"
    }
    channel.send(result_ch, body)
  })

  match channel.receive(result_ch) {
    channel.Message(body) -> body
    _ -> "the channel closed"
  }
}
"#;
        run(input)
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
