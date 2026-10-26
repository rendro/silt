//! `process.exit` and `io.eprint` under the `silt` command, where the
//! pipes and the process's end are what is tested: which of a failure
//! and an exit ends the run, what a slow reader is given, and a stderr
//! whose reader has gone.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Write `src` to a file of its own under the temp directory.
fn program(label: &str, src: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "silt_process_exit_{label}_{}.silt",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write temp file");
    path
}

/// `silt run <path>`, its output captured.
fn run(path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(path)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn silt")
}

/// A failure of `main` and a task's `process.exit` race: whichever
/// comes first is the run's end, with its own report and code. A run
/// never reports the failure and exits with the exit's code, nor exits
/// with 1 and says nothing. Each program is run many times, so both
/// orders come up.
#[test]
fn a_failure_and_an_exit_that_race_end_the_run_one_way() {
    let cases = [
        (
            "panic",
            "import process\nimport task\nfn main() {\n  let _ = task.spawn { -> process.exit(0) }\n  panic(\"main boom\")\n}\n",
            "error[runtime]: panic: main boom",
            0,
        ),
        (
            "err",
            "import process\nimport task\nfn main() -> Result((), String) {\n  let _ = task.spawn { -> process.exit(3) }\n  Err(\"bad input\")\n}\n",
            "error[runtime]: main returned Err: bad input",
            3,
        ),
    ];
    for (label, src, report, code) in cases {
        let path = program(label, src);
        for _ in 0..40 {
            let out = run(&path);
            let stderr = String::from_utf8_lossy(&out.stderr);
            match out.status.code() {
                Some(1) => assert!(
                    stderr.starts_with(report),
                    "{label}: exit 1 with {stderr:?}"
                ),
                Some(c) if c == code => assert_eq!(stderr, "", "{label}: exit {c} with a report"),
                other => panic!("{label}: exit {other:?}, stderr {stderr:?}"),
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// What the program printed before `process.exit` reaches a reader that
/// takes its time, all of it, as at a normal end: here the text
/// `print` left without a line end after a write that filled the pipe.
#[test]
fn an_exit_writes_out_what_was_printed_to_a_slow_reader() {
    for (label, call) in [
        ("main", "process.exit(3)"),
        (
            "task",
            "let _ = task.join(task.spawn { -> process.exit(3) })",
        ),
    ] {
        let src = format!(
            "import process\nimport string\nimport task\nfn main() {{\n  println(string.repeat(\"x\", 65535))\n  print(\"TAIL\")\n  {call}\n}}\n"
        );
        let path = program(&format!("slow_{label}"), &src);
        let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
            .arg("run")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn silt");
        // Longer than the wait for another task's write.
        std::thread::sleep(Duration::from_millis(1500));
        let mut text = String::new();
        child
            .stdout
            .take()
            .expect("stdout")
            .read_to_string(&mut text)
            .expect("read stdout");
        let status = child.wait().expect("wait for silt");
        let _ = std::fs::remove_file(&path);
        assert_eq!(status.code(), Some(3), "{label}");
        assert_eq!(text.len(), 65540, "{label}");
        assert!(text.ends_with("x\nTAIL"), "{label}");
    }
}

/// A write to a stderr whose reader has gone is dropped: the program
/// goes on, and ends as it would have. (Unix: there such a write raises
/// SIGPIPE, which the `silt` command does not ignore.)
#[cfg(unix)]
#[test]
fn a_write_to_a_stderr_whose_reader_has_gone_is_ignored() {
    let src = "import io\nimport string\nfn main() {\n  let _ = io.read_line()\n  io.eprintln(string.repeat(\"e\", 1000))\n  io.eprint(\"more\")\n  println(\"after\")\n}\n";
    let path = program("closed_stderr", src);
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn silt");
    // The reader of stderr goes before the program writes to it.
    drop(child.stderr.take());
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"go\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for silt");
    let _ = std::fs::remove_file(&path);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
}
