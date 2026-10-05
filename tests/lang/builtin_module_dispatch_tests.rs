//! Every builtin module that is built runs its functions: a call
//! `module.func(...)` finds its row in the builtin registry and never
//! fails with `unknown builtin namespace: <module>`. This test calls one
//! cheap, side-effect-free function of every module through `silt run`.
//!
//! It also locks the feature gating of `tcp`, a default feature: with it
//! off, `silt check` rejects the module's import with one error that
//! names the feature, and says nothing more about what the program takes
//! from the module. (For `postgres`, which the default build lacks, that
//! is the golden case `lang/imports/feature__import_of_a_module_whose_
//! feature_is_not_built`.)

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

/// `silt check` on `src`, which imports the builtin module `module` of
/// the cargo feature `feature` that is not built, fails with one error,
/// at the import, that names the feature.
#[allow(dead_code)]
fn assert_needs_feature(label: &str, src: &str, module: &str, feature: &str) {
    let out = silt("check", label, src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`silt check` accepted `import {module}` in a build without `{feature}`:\n{src}"
    );
    let wanted = format!(
        "the builtin module '{module}' is not part of this build of silt: it needs the cargo \
         feature `{feature}`"
    );
    assert!(
        stderr.contains(&format!("error[compile]: {wanted}")),
        "`silt check` must name the feature at the import; got:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("rebuild silt with `--features {feature}`")),
        "the error says how to get the module; got:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("error[").count(),
        1,
        "one error, at the import; got:\n{stderr}"
    );
}

#[cfg(not(feature = "tcp"))]
#[test]
fn importing_tcp_without_its_feature_is_one_error_that_names_it() {
    assert_needs_feature(
        "tcp_connect",
        "import tcp\nfn main() {\n  let e = tcp.TcpError.TcpConnect(\"nope\")\n  println(e)\n}\n",
        "tcp",
        "tcp",
    );
}
