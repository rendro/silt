//! A write to stdout that fails (a full disk: `/dev/full`) is the
//! program's failure, also when the print had returned before the write
//! was made (the process's output goes through a writer) and the program
//! then ends by `process.exit(0)`.

#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};

/// Run `source` with stdout on `/dev/full`: (status, stderr).
fn run_into_a_full_disk(name: &str, source: &str) -> (Option<i32>, String) {
    let dir =
        std::env::temp_dir().join(format!("silt_stdout_failure_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, source).unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full");
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::null())
        .stdout(full)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn a_failed_print_before_process_exit_is_the_programs_failure() {
    let (code, err) = run_into_a_full_disk(
        "exit",
        "import process\n\nfn main() {\n  println(\"one line\")\n  process.exit(0)\n}\n",
    );
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("cannot write to stdout: No space left on device"),
        "{err}"
    );
}

#[test]
fn a_failed_print_at_the_end_of_main_is_the_programs_failure() {
    let (code, err) = run_into_a_full_disk("end", "fn main() {\n  println(\"one line\")\n}\n");
    assert_eq!(code, Some(1), "{err}");
    assert!(
        err.contains("cannot write to stdout: No space left on device"),
        "{err}"
    );
}
