//! `silt test` over several test files that import one module with an
//! error shows the module's error once, as it shows its warnings once;
//! each file still says it failed to compile. FAIL on the base commit:
//! the error was printed once for each test file.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_test_shared_error_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn an_error_of_a_module_three_test_files_import_is_shown_once() {
    let dir = fresh_dir();
    fs::write(
        dir.join("shared.silt"),
        "pub fn three() -> Int {\n  \"3\"\n}\n",
    )
    .unwrap();
    for name in ["a", "b", "c"] {
        fs::write(
            dir.join(format!("{name}_test.silt")),
            "import shared\nimport test\n\nfn test_three() {\n  test.assert_eq(shared.three(), 3)\n}\n",
        )
        .unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("test")
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let headlines = (stderr.lines()).filter(|line| line.starts_with("error[type]: type mismatch"));
    assert_eq!(headlines.count(), 1, "{stderr}");
    assert_eq!(stderr.matches("--> shared.silt:2:3").count(), 1, "{stderr}");
    assert_eq!(
        stderr.matches(": failed to compile —").count(),
        3,
        "{stderr}"
    );
}
