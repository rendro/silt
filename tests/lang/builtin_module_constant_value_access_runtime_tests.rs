//! Every constant of a builtin module (`math.pi`, `float.epsilon`, the
//! constant rows of the builtin registry) is a value at run time: a
//! program that names one as a bare value runs, and does not fail with
//! `undefined global: <module>.<const>`.
//!
//! `every_registry_constant_is_value_accessible` runs a real `silt run`
//! program for each constant `module::builtin_module_constants` lists.

use std::process::Command;

/// Run a silt source program via the `silt run` subcommand and return
/// `(stdout, stderr, success)`. The temp file path includes both the
/// caller label AND the test process id / thread id so parallel tests
/// in this same binary cannot clobber each other's source files.
fn run_silt_raw(label: &str, src: &str) -> (String, String, bool) {
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    let tid = tid
        .trim_start_matches("ThreadId(")
        .trim_end_matches(')')
        .to_string();
    let tmp = std::env::temp_dir().join(format!("silt_const_access_rt_{label}_p{pid}_t{tid}.silt"));
    std::fs::write(&tmp, src).expect("write temp file");
    let bin = env!("CARGO_BIN_EXE_silt");
    let out = Command::new(bin)
        .arg("run")
        .arg(&tmp)
        .output()
        .expect("spawn silt run");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_file(&tmp);
    (stdout, stderr, out.status.success())
}

#[test]
fn every_registry_constant_is_value_accessible() {
    let mut checked = 0usize;
    for &module in silt::module::builtin_modules() {
        for konst in silt::module::builtin_module_constants(module) {
            checked += 1;
            let label = format!("{module}_{konst}");
            // Bind the constant as a first-class value (forces a
            // `GetGlobal("<module>.<const>")` lookup) and print a derived
            // marker so the program both compiles AND runs to completion.
            // We intentionally don't print the raw float (NaN/inf format
            // differences are irrelevant here) — the marker proves the
            // global resolved and the VM reached the end of `main`.
            let src = format!(
                r#"
import {module}
fn main() {{
  let c = {module}.{konst}
  let _ = c
  println("OK:{module}.{konst}")
}}
"#
            );
            let (stdout, stderr, ok) = run_silt_raw(&label, &src);
            let undef = format!("undefined global: {module}.{konst}");
            assert!(
                !stderr.contains(&undef),
                "the constant `{module}.{konst}` of the builtin registry is \
                 not a value at run time.\nstderr={stderr:?}"
            );
            assert!(
                ok,
                "first-class value access of registry constant \
                 `{module}.{konst}` failed at runtime.\n\
                 stdout={stdout:?}\nstderr={stderr:?}"
            );
            assert!(
                stdout.contains(&format!("OK:{module}.{konst}")),
                "expected `OK:{module}.{konst}` in stdout; got {stdout:?}"
            );
        }
    }
    // Guard against the enumeration silently becoming empty (e.g. a
    // refactor that makes `builtin_module_constants` return `vec![]` for
    // everything would otherwise make this test vacuously pass).
    assert!(
        checked >= 6,
        "expected at least the 6 known module constants (math.pi/e + 4 \
         float.*); only enumerated {checked} — registry regressed?"
    );
}
