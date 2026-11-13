//! `silt fmt -` and `silt check -` read the program from stdin (`fmt`
//! writes the result to stdout); `silt check` takes several files and
//! checks each. FAIL on the base commit: `-` was an unknown flag, and a
//! second file was an unexpected extra argument.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fresh_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_stdin_{tag}_{}_{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `silt <args>` in `dir`, with `stdin` on its stdin.
fn silt(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A command refused before it reads stdin may have ended already:
    // writing to it then fails with a broken pipe, which is no fault.
    let written = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    if let Err(e) = written {
        assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
    }
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn fmt_dash_formats_stdin_to_stdout() {
    let dir = fresh_dir("fmt");
    let out = silt(&dir, &["fmt", "-"], "fn main(){println(\"x\")}\n");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(text(&out.stdout), "fn main() {\n  println(\"x\")\n}\n");
    assert!(out.stderr.is_empty(), "{out:?}");
    // Nothing is written to a file.
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);

    // --check: nothing on stdout; 1 when not formatted, 0 when it is.
    let out = silt(
        &dir,
        &["fmt", "--check", "-"],
        "fn main(){println(\"x\")}\n",
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(text(&out.stderr), "<stdin>: not formatted\n");
    let out = silt(
        &dir,
        &["fmt", "--check", "-"],
        "fn main() {\n  println(\"x\")\n}\n",
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");

    // A syntax error is reported at `<stdin>`, exit 2, nothing on stdout.
    let out = silt(&dir, &["fmt", "-"], "fn main() {\n  let = 1\n}\n");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(text(&out.stderr).contains(" --> <stdin>:2:"), "{out:?}");
}

#[test]
fn check_dash_checks_stdin() {
    let dir = fresh_dir("check");
    // A module beside it is imported as from a file of the directory.
    fs::write(dir.join("helper.silt"), "pub fn two() -> Int {\n  2\n}\n").unwrap();
    let out = silt(
        &dir,
        &["check", "-"],
        "import helper\n\nfn main() {\n  println(helper.two())\n}\n",
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");

    let program = "fn main() {\n  let x: Int = \"s\"\n  println(x)\n}\n";
    let out = silt(&dir, &["check", "-"], program);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with(
            "error[type]: type mismatch: expected Int, got String\n --> <stdin>:2:16\n"
        ),
        "{stderr}"
    );

    let out = silt(&dir, &["check", "--format", "json", "-"], program);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json[0]["file"], "<stdin>", "{json}");
    assert_eq!(json[0]["line"], 2, "{json}");
}

#[test]
fn dash_takes_no_other_file() {
    let dir = fresh_dir("alone");
    fs::write(dir.join("a.silt"), "fn main() { () }\n").unwrap();
    for command in ["fmt", "check"] {
        let out = silt(&dir, &[command, "-", "a.silt"], "fn main() { () }\n");
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert_eq!(
            text(&out.stderr),
            format!(
                "error: silt {command}: '-' reads the program from stdin, and takes no other file\n\
                 Run 'silt {command} --help' for usage.\n"
            )
        );
    }
}

#[test]
fn check_takes_several_files() {
    let dir = fresh_dir("many");
    fs::write(dir.join("ok.silt"), "fn main() {\n  println(\"x\")\n}\n").unwrap();
    fs::write(
        dir.join("bad.silt"),
        "fn main() {\n  let x: Int = \"s\"\n  println(x)\n}\n",
    )
    .unwrap();
    // Both are checked: the error of the second fails the run.
    let out = silt(&dir, &["check", "ok.silt", "bad.silt"], "");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out.stderr).contains(" --> bad.silt:2:16\n"),
        "{out:?}"
    );
    let out = silt(&dir, &["check", "ok.silt", "ok.silt"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    // A file that cannot be read is reported; the others are checked.
    let out = silt(&dir, &["check", "missing.silt", "bad.silt"], "");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("error reading missing.silt: "),
        "{stderr}"
    );
    assert!(stderr.contains(" --> bad.silt:2:16\n"), "{stderr}");

    // One JSON array for all of them.
    let out = silt(
        &dir,
        &["check", "--format", "json", "missing.silt", "bad.silt"],
        "",
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let files: Vec<&str> = (json.as_array().unwrap().iter())
        .map(|d| d["file"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["missing.silt", "bad.silt"], "{json}");
}

#[cfg(feature = "watch")]
#[test]
fn check_watch_refuses_stdin() {
    let dir = fresh_dir("watch");
    let out = silt(&dir, &["check", "--watch", "-"], "fn main() { () }\n");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr),
        "error: silt check: --watch cannot watch stdin ('-'): name the files\n"
    );
}
