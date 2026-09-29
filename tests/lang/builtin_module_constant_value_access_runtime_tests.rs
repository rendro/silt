//! Round-95 follow-up RUNTIME parity lock — every constant in the
//! `module::builtin_module_constants` registry must be seeded as a
//! first-class VALUE global by `Vm::register_builtins`
//! (`src/vm/dispatch.rs`), reachable at runtime without crashing with
//! `error[runtime]: undefined global: <module>.<const>`.
//!
//! ## Background
//!
//! Module constants (`math.pi`, `math.e`, `float.epsilon`,
//! `float.infinity`, …) live hand-encoded across FOUR parallel surfaces:
//!
//! 1. typechecker schemes   — `src/typechecker/builtins/float.rs`,
//!    `.../math.rs` (`intern("math.pi")`, …);
//! 2. VM globals            — `src/vm/dispatch.rs::register_builtins`
//!    (nine hand-written `self.globals.insert("math.pi", Value::Float(…))`
//!    lines);
//! 3. registry              — `module::builtin_module_constants`
//!    (`src/module.rs`);
//! 4. docs table            — `src/typechecker/builtins/docs.rs`.
//!
//! The *function* path is registry-driven: `register_builtins` loops
//! `module::builtin_module_functions(m)` for every `BUILTIN_MODULES`
//! entry, so a typechecker function name with no matching VM global is
//! impossible to introduce without tripping
//! `builtin_module_function_value_access_runtime_tests` /
//! `comprehensive_module_function_parity_tests`.
//!
//! The *constant* path is NOT registry-driven — the inserts at
//! `dispatch.rs:248-295` are nine literal lines decoupled from
//! `builtin_module_constants`. The comprehensive function-parity test
//! only closes the typechecker→registry direction (it asserts every
//! typechecker name appears in `functions ∪ constants`). NOTHING closed
//! the registry→runtime direction for constants: a 10th constant added
//! to `builtin_module_constants("math")` plus a typechecker scheme but
//! NOT to `register_builtins` would pass `silt check`, offer in
//! LSP/REPL completion, keep the comprehensive parity test GREEN — yet
//! `fn main() { math.tau }` would crash at runtime with
//! `undefined global: math.tau`, the exact round-72 value-access class.
//!
//! ## Lock
//!
//! `every_registry_constant_is_value_accessible` enumerates
//! `module::builtin_module_constants(m)` for EVERY `m` in
//! `module::BUILTIN_MODULES` and runs a real `silt run` program that
//! references `<module>.<const>` as a bare value. The assertion is that
//! the program neither fails nor surfaces the canonical
//! `undefined global: <module>.<const>` wording. Because the source of
//! truth for the enumeration is the registry itself, adding a constant
//! to `builtin_module_constants` without seeding it as a VM global makes
//! THIS test fail (`undefined global`) — closing the
//! registry→runtime direction that the function-parity tests close for
//! functions.

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
    for &module in silt::module::BUILTIN_MODULES {
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
                "registry constant `{module}.{konst}` is NOT seeded as a VM \
                 value global — `Vm::register_builtins` in src/vm/dispatch.rs \
                 must `self.globals.insert(\"{module}.{konst}\", …)`. This is \
                 the round-72 undefined-global class re-opening through the \
                 hand-coded constant path.\nstderr={stderr:?}"
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
        checked >= 9,
        "expected at least the 9 known module constants (math.pi/e + 7 \
         float.*); only enumerated {checked} — registry regressed?"
    );
}
