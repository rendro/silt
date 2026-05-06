//! Round-74 DOC fix #4: `docs/proposals/effect-rows.md` uses real silt
//! surface syntax.
//!
//! Round-73 L3 had asserted `EFFECT_DOC_SRC.contains("env.home() + \"")`
//! and `EFFECT_DOC_SRC.contains(".unwrap_or(")`, but `env.home()` does
//! not exist (only `env.get`, `env.set`, `env.remove`, `env.vars`) and
//! `.unwrap_or(...)` is not a method on `Result` / `Option` (the stdlib
//! exposes module-qualified `result.unwrap_or` / `option.unwrap_or`).
//!
//! Round-74 rewrites the snippets at lines 67-78 and 372-389 to use
//! the real surface, and updates the round-73 L3 lock test to assert
//! the corrected forms (see `tests/round73_doc_drift_tests.rs::l3_*`).

use std::path::{Path, PathBuf};
use std::process::Command;

const EFFECT_DOC_SRC: &str = include_str!("../docs/proposals/effect-rows.md");

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round74_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

fn check_ok(dir_suffix: &str, src: &str) {
    let dir = scratch_dir(dir_suffix);
    let main = write_main(&dir, src);
    let output = silt_bin()
        .args(["check", main.to_str().unwrap()])
        .output()
        .expect("silt check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "silt check should succeed for {dir_suffix} snippet.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn finding4_no_env_home_call() {
    // `env.home()` is not a real stdlib function. The corrected snippet
    // calls `env.get("HOME")` (returns `Option(String)`) and unwraps it
    // via `option.unwrap_or`.
    assert!(
        !EFFECT_DOC_SRC.contains("env.home("),
        "effect-rows.md must not call the non-existent `env.home()` — \
         use `env.get(\"HOME\")` + `option.unwrap_or(...)` instead."
    );
}

#[test]
fn finding4_no_bare_method_unwrap_or() {
    // Old broken form: `value.unwrap_or(default)` as a method call. The
    // stdlib only exposes `option.unwrap_or` / `result.unwrap_or` as
    // free functions (or module-qualified `option.unwrap_or` /
    // `result.unwrap_or`), not as receiver methods on the values.
    //
    // The doc must not contain the suspicious method-call form
    // `).unwrap_or(` (where `)` immediately precedes `.unwrap_or`,
    // which is the diagnostic signature of "trying to chain
    // `.unwrap_or` directly off a Result/Option expression").
    assert!(
        !EFFECT_DOC_SRC.contains(").unwrap_or("),
        "effect-rows.md must not call `.unwrap_or(...)` as a method on \
         a Result/Option expression — silt only exposes module-qualified \
         `result.unwrap_or(r, default)` / `option.unwrap_or(o, default)`."
    );
}

#[test]
fn finding4_uses_module_qualified_unwrap_or() {
    // Positive shape: at least one of `option.unwrap_or(` or
    // `result.unwrap_or(` must be present.
    assert!(
        EFFECT_DOC_SRC.contains("option.unwrap_or(")
            || EFFECT_DOC_SRC.contains("result.unwrap_or("),
        "effect-rows.md should use module-qualified `option.unwrap_or` / \
         `result.unwrap_or` instead of bare `.unwrap_or(...)`."
    );
    // Both modules are referenced because the corrected snippet has
    // both an `Option` (from `env.get`) and `Result` (from `io.read_file`,
    // `toml.parse`) value to unwrap.
    assert!(
        EFFECT_DOC_SRC.contains("option.unwrap_or("),
        "effect-rows.md should call `option.unwrap_or` for the `env.get` Option."
    );
    assert!(
        EFFECT_DOC_SRC.contains("result.unwrap_or("),
        "effect-rows.md should call `result.unwrap_or` for Result values."
    );
}

#[test]
fn finding4_corrected_snippet_typechecks() {
    // Lift the corrected snippet (cribbed from the doc lines 67-74 / 372-389)
    // through `silt check`.
    let src = r#"
import env
import io
import toml
import option
import result

type Config { name: String }

fn default_config() -> Config { Config { name: "default" } }

fn load_defaults() -> Config {
  let home = option.unwrap_or(env.get("HOME"), "/")
  let path = home + "/.config/app.toml"
  let raw = result.unwrap_or(io.read_file(path), "")
  result.unwrap_or(toml.parse(raw), default_config())
}

fn main() { let _ = load_defaults() }
"#;
    check_ok("finding4_effect_rows", src);
}
