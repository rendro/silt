//! Every builtin module must reach its runtime dispatch arm. The VM routes
//! `module.func(...)` calls by namespace in `src/vm/dispatch.rs`; a module
//! listed in `module::BUILTIN_MODULES` whose arm is missing (or gated off
//! while the typechecker still accepts the call) fails at runtime with
//! `unknown builtin namespace: <module>`. This test calls one cheap,
//! side-effect-free function of every module through `silt run`.
//!
//! It also locks the feature gating of the builtin error enums: with the
//! `postgres` / `tcp` feature off, `silt check` must reject `PgError` /
//! `TcpError` uses as undefined instead of letting them reach the runtime,
//! where they would fail with `unknown builtin namespace`.

use std::process::{Command, Output};

/// One call per builtin module; `None` for a module whose cargo feature is
/// off in this build (the module is then not callable and not tested).
fn module_call(module: &str) -> Option<Option<&'static str>> {
    let call = match module {
        "io" => "io.args()",
        "string" => "string.length(\"abc\")",
        "int" => "int.abs(-1)",
        "float" => "float.abs(-1.5)",
        "list" => "list.length([1, 2])",
        "map" => "map.length(#{\"a\": 1})",
        "result" => "result.is_ok(Ok(1))",
        "option" => "option.is_some(Some(1))",
        "test" => "test.assert(true)",
        "channel" => "channel.new(1)",
        "task" => "task.spawn({ -> 1 })",
        "regex" => "regex.is_match(\"a+\", \"aa\")",
        "json" => "json.stringify(1)",
        "toml" => "toml.parse_map(\"a = 1\", Int)",
        "set" => "set.new()",
        "math" => "math.sqrt(4.0)",
        "time" => "time.hours(1)",
        "fs" => "fs.exists(\"silt_no_such_file_xyz\")",
        "env" => "env.get(\"SILT_NO_SUCH_VAR_XYZ\")",
        "bytes" => "bytes.empty()",
        "crypto" => "crypto.random_bytes(4)",
        "encoding" => "encoding.url_encode(\"a b\")",
        "stream" => "stream.from_list([1])",
        "uuid" => "uuid.nil()",
        "http" if cfg!(feature = "http") => "http.segments(\"/a/b\")",
        "postgres" if cfg!(feature = "postgres") => "postgres.uuidv7()",
        // Port 1 on loopback refuses at once; the call returns an Err.
        "tcp" if cfg!(feature = "tcp") => "tcp.connect(\"127.0.0.1:1\")",
        "http" | "postgres" | "tcp" => return Some(None),
        _ => return None,
    };
    Some(Some(call))
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let tid = format!("{:?}", std::thread::current().id());
    let tid: String = tid.chars().filter(|c| c.is_ascii_digit()).collect();
    std::env::temp_dir().join(format!(
        "silt_builtin_dispatch_{label}_p{}_t{tid}.silt",
        std::process::id()
    ))
}

fn silt(sub: &str, label: &str, src: &str) -> Output {
    let path = temp_path(label);
    std::fs::write(&path, src).expect("write temp file");
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(sub)
        .arg(&path)
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn silt");
    let _ = std::fs::remove_file(&path);
    out
}

#[test]
fn every_builtin_module_reaches_its_dispatch_arm() {
    let mut missing = Vec::new();
    for &module in silt::module::builtin_modules() {
        let Some(call) = module_call(module) else {
            missing.push(module);
            continue;
        };
        let Some(call) = call else { continue };
        let src = format!(
            "import {module}\nfn main() {{\n  let _ = {call}\n  println(\"OK:{module}\")\n}}\n"
        );
        let out = silt("run", module, &src);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("unknown builtin namespace"),
            "`{call}` did not reach the `{module}` dispatch arm:\n{stderr}"
        );
        assert!(
            out.status.success() && stdout.contains(&format!("OK:{module}")),
            "`{call}` failed:\nstdout={stdout}\nstderr={stderr}"
        );
    }
    assert!(
        missing.is_empty(),
        "builtin modules with no call in `module_call`; add one: {missing:?}"
    );
}

/// `silt check` on `src` must fail and name `name` as undefined.
#[allow(dead_code)]
fn assert_undefined(label: &str, src: &str, name: &str) {
    let out = silt("check", label, src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`silt check` accepted `{name}` in a build without its feature:\n{src}"
    );
    assert!(
        stderr.contains("undefined") && stderr.contains(name),
        "`silt check` must report `{name}` as undefined; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("unknown builtin namespace"),
        "`{name}` reached the runtime:\n{stderr}"
    );
}

#[cfg(not(feature = "postgres"))]
#[test]
fn pg_error_is_undefined_without_postgres_feature() {
    assert_undefined(
        "pg_timeout",
        "import postgres\nfn main() {\n  let e = postgres.PgTimeout\n  println(e)\n}\n",
        "PgTimeout",
    );
    assert_undefined(
        "pg_connect",
        "import postgres\nfn main() {\n  let e = postgres.PgError.PgConnect(\"nope\")\n  println(e)\n}\n",
        "PgError",
    );
}

#[cfg(not(feature = "tcp"))]
#[test]
fn tcp_error_is_undefined_without_tcp_feature() {
    assert_undefined(
        "tcp_timeout",
        "import tcp\nfn main() {\n  let e = tcp.TcpTimeout\n  println(e)\n}\n",
        "TcpTimeout",
    );
    assert_undefined(
        "tcp_connect",
        "import tcp\nfn main() {\n  let e = tcp.TcpError.TcpConnect(\"nope\")\n  println(e)\n}\n",
        "TcpError",
    );
}
