//! A 0-byte source file must be handled by every file subcommand without
//! a panic or a hang: `run` and `check` report the missing `main()`,
//! `test` finds no tests, and `disasm` prints the (prelude-only) script.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt_empty_file_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `silt <sub> empty.silt` in `dir`, killing it after 20 s. Output goes
/// to files, not pipes: `disasm` prints far more than a pipe buffer holds.
fn run_silt(dir: &Path, sub: &str) -> (Option<i32>, String, String) {
    let out_path = dir.join(format!("{sub}.out"));
    let err_path = dir.join(format!("{sub}.err"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args([sub, "empty.silt"])
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .stdin(Stdio::null())
        .stdout(fs::File::create(&out_path).unwrap())
        .stderr(fs::File::create(&err_path).unwrap())
        .spawn()
        .expect("failed to spawn silt");
    let start = Instant::now();
    let status = loop {
        match child.try_wait().expect("try_wait failed") {
            Some(status) => break status,
            None if start.elapsed() >= Duration::from_secs(20) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("silt {sub} on a 0-byte file did not exit within 20 s");
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    };
    let stdout = fs::read_to_string(&out_path).unwrap();
    let stderr = fs::read_to_string(&err_path).unwrap();
    (status.code(), stdout, stderr)
}

#[test]
fn empty_file_run_check_test_disasm() {
    let dir = fresh_dir();
    fs::write(dir.join("empty.silt"), "").unwrap();

    // (subcommand, expected exit, needle, needle is in stdout (else stderr))
    let cases = [
        ("run", 1, "no main()", false),
        ("check", 1, "no main()", false),
        ("test", 0, "0 tests", false),
        ("disasm", 0, "<script>", true),
    ];
    for (sub, exit, needle, in_stdout) in cases {
        let (code, stdout, stderr) = run_silt(&dir, sub);
        assert_eq!(
            code,
            Some(exit),
            "silt {sub} on a 0-byte file: exit\nstdout={stdout}\nstderr={stderr}"
        );
        let haystack = if in_stdout { &stdout } else { &stderr };
        assert!(
            haystack.contains(needle),
            "silt {sub} on a 0-byte file: expected {needle:?}\nstdout={stdout}\nstderr={stderr}"
        );
        assert!(
            !stderr.contains("panicked at"),
            "silt {sub} on a 0-byte file panicked:\n{stderr}"
        );
    }

    let _ = fs::remove_dir_all(&dir);
}
