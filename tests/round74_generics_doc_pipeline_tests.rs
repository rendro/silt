//! Round-74 DOC fix #3: corrected generics.md "Pipe interaction"
//! snippet typechecks.
//!
//! Original broken snippet at `docs/language/generics.md:287-292`:
//!
//! ```silt
//! raw_bytes
//! |> bytes.to_string
//! |> result.map_ok(fn(s) { json.parse(s, Config) })
//! |> result.flatten
//! ```
//!
//! `bytes.to_string` returns `Result(String, BytesError)`; `json.parse`
//! returns `Result(_, JsonError)`. After `result.map_ok(json.parse)`
//! the value is `Result(Result(Config, JsonError), BytesError)`, but
//! `result.flatten` requires both error types to match.
//!
//! The corrected snippet wraps each step's error in a shared `LoadError`
//! enum and uses `result.map_err` + `?` instead of `result.flatten`.

use std::path::{Path, PathBuf};
use std::process::Command;

const GENERICS_DOC_SRC: &str = include_str!("../docs/language/generics.md");

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
fn finding3_old_broken_pipeline_is_gone() {
    // The old broken pipeline used `result.map_ok` + `result.flatten`
    // across mismatched error types. The corrected doc must not
    // resurrect it.
    assert!(
        !GENERICS_DOC_SRC.contains("|> result.flatten"),
        "generics.md must not pipe into `result.flatten` — \
         `bytes.to_string`'s `BytesError` and `json.parse`'s `JsonError` \
         do not unify."
    );
}

#[test]
fn finding3_corrected_pipeline_uses_map_err_pattern() {
    // Positive shape: the corrected snippet introduces a unified
    // `LoadError` enum and pipes through `result.map_err`.
    assert!(
        GENERICS_DOC_SRC.contains("type LoadError"),
        "generics.md should introduce a unified `LoadError` enum so the \
         pipeline composes through `?`."
    );
    assert!(
        GENERICS_DOC_SRC.contains("result.map_err(Decode)"),
        "generics.md pipeline should wrap `BytesError` via `result.map_err(Decode)`."
    );
    assert!(
        GENERICS_DOC_SRC.contains("result.map_err(Parse)"),
        "generics.md pipeline should wrap `JsonError` via `result.map_err(Parse)`."
    );
}

#[test]
fn finding3_corrected_pipeline_typechecks() {
    let src = r#"
import bytes
import json
import result

type Config { name: String }

type LoadError {
  Decode(BytesError),
  Parse(JsonError),
}

fn load_config(raw_bytes) -> Result(Config, LoadError) {
  let s = raw_bytes |> bytes.to_string |> result.map_err(Decode)?
  json.parse(s, Config) |> result.map_err(Parse)
}

fn main() { () }
"#;
    check_ok("finding3_pipeline", src);
}
