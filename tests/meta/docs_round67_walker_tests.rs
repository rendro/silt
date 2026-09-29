//! Round-67 DOC agent locks for the operators / types / generics /
//! strict-effects-migration finding cluster.
//!
//! F16: the `docs/strict-effects-migration.md` "After" worked example
//! annotates BOTH `load_settings` and `main`. Walker runs `silt check
//! --strict-effects` and asserts no errors. The other walkers of this
//! round are golden cases under `tests/golden/meta/docs/`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn silt_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_silt"))
}

fn scratch_dir(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_round67_{suffix}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_main(dir: &Path, src: &str) -> PathBuf {
    let path = dir.join("main.silt");
    std::fs::write(&path, src).unwrap();
    path
}

// ────────────────────────────────────────────────────────────────────
// F16: strict-effects-migration "After" example actually goes clean
// ────────────────────────────────────────────────────────────────────

#[test]
fn f16_migration_after_example_passes_strict_effects_in_one_pass() {
    let dir = scratch_dir("f16_strict");
    // Mirror the doc's "After" example verbatim.
    let src = "import io\n\
        \n\
        fn load_settings(path: String) -> Result(String, IoError) !{fs, io} =\n\
        \x20\x20io.read_file(path)\n\
        \n\
        fn main() !{fs, io} {\n\
        \x20\x20let _settings = load_settings(\"config.toml\")\n\
        \x20\x20()\n\
        }\n";
    let main = write_main(&dir, src);

    let output = silt_bin()
        .args(["check", "--strict-effects", main.to_str().unwrap()])
        .output()
        .expect("silt check --strict-effects");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "the migration `After` example must typecheck under \
         `silt check --strict-effects` in one pass.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // No error[type]: lines should appear.
    assert!(
        !stderr.contains("error[type]:"),
        "--strict-effects on the After example should produce zero \
         type errors; got stderr:\n{stderr}"
    );
}
