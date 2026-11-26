//! `silt fmt -` and `silt check -` read the program from stdin (`fmt`
//! writes the result to stdout); `silt check` and `silt test` take
//! several paths and read each. FAIL on the base commit: `-` was an
//! unknown flag, and a second file was an unexpected extra argument.

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

/// A module that one program imports and that is checked as a program of
/// its own too (`silt check .` of a directory of both, or both named, the
/// module by another spelling of its path) has each error said once, as
/// the first program that found it shows it: `check .` checks `./m.silt`
/// first, `check main.silt ./m.silt` finds it as main's import, with its
/// "imported here". FAIL on the base: the import's error carries that
/// label, so it was told apart from the module's own by its text and
/// shown twice (also as `./m.silt` beside `m.silt`).
#[test]
fn check_says_an_error_of_a_module_once_when_two_programs_find_it() {
    let dir = fresh_dir("twice");
    fs::write(
        dir.join("main.silt"),
        "import m\n\nfn main() {\n  println(m.add(3, 4))\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("m.silt"),
        "pub fn add(a, b) = a + b\n\npub fn square(x) = x * x\n",
    )
    .unwrap();
    let dot = format!(".{}m.silt", std::path::MAIN_SEPARATOR);
    let cases = [
        (vec!["check", "."], "m.silt", 0),
        (vec!["check", "main.silt", dot.as_str()], "m.silt", 2),
    ];
    for (args, file, imported) in cases {
        let out = silt(&dir, &args, "");
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
        let stderr = text(&out.stderr).replace('\\', "/");
        let places: Vec<&str> = (stderr.lines())
            .filter(|line| line.starts_with(" --> "))
            .collect();
        let expected = [format!(" --> {file}:1:18"), format!(" --> {file}:3:18")];
        assert_eq!(places, expected, "{args:?}: {stderr}");
        assert_eq!(
            stderr.matches("module 'm' is imported here").count(),
            imported,
            "{args:?}: {stderr}"
        );
    }
    let out = silt(&dir, &["check", "--format", "json", "."], "");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 2, "{json}");
}

/// `silt check` of a directory names each file one way: relative to
/// where the command was given, without a leading `./`, as an import
/// names it. FAIL on 478805d4: `check .` named the files it found
/// `./b.silt`, and the import of them `b.silt`, in one output.
#[test]
fn check_of_a_directory_names_each_file_one_way() {
    let dir = fresh_dir("spelling");
    fs::write(
        dir.join("main.silt"),
        "import b\nfn main() { println(1) }\n",
    )
    .unwrap();
    fs::write(dir.join("b.silt"), "import c\npub fn f() -> Int { 1 }\n").unwrap();
    fs::write(dir.join("c.silt"), "import b\npub fn g() -> Int { 1 }\n").unwrap();
    let out = silt(&dir, &["check", "."], "");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = text(&out.stderr).replace('\\', "/");
    let mut places: Vec<&str> = (stderr.lines())
        .filter_map(|line| line.strip_prefix(" --> "))
        .collect();
    places.dedup();
    assert_eq!(
        places,
        ["b.silt:1:1", "c.silt:1:1", "main.silt:1:1"],
        "{stderr}"
    );
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

/// `silt test` takes several paths, files and directories, runs the
/// tests of each with one summary, runs a file given twice once, and
/// refuses a path that is neither before any test runs. FAIL on the
/// base commit: a second path was an unexpected extra argument.
#[test]
fn test_takes_several_paths() {
    let dir = fresh_dir("test_paths");
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::write(dir.join("a/x_test.silt"), "fn test_one() {\n  ()\n}\n").unwrap();
    fs::write(dir.join("b_test.silt"), "fn test_two() {\n  ()\n}\n").unwrap();
    fs::write(dir.join("prog.txt"), "hi\n").unwrap();

    let out = silt(&dir, &["test", "a", "b_test.silt"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    // (The results are written to stderr, each file as the system writes
    // its path: `a\x_test.silt` on Windows.)
    let results = text(&out.stderr).replace('\\', "/");
    assert!(results.contains("PASS a/x_test.silt::test_one"), "{out:?}");
    assert!(results.contains("PASS b_test.silt::test_two"), "{out:?}");
    assert!(results.contains("2 tests: 2 passed"), "{out:?}");

    // One file, two spellings: run once.
    let out = silt(&dir, &["test", "b_test.silt", "./b_test.silt"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(text(&out.stderr).contains("1 test: 1 passed"), "{out:?}");

    // A path that is no .silt file and no directory: nothing runs.
    let out = silt(&dir, &["test", "a", "prog.txt"], "");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out.stderr).contains("silt test: prog.txt is not a .silt file or a directory"),
        "{out:?}"
    );
    assert!(!text(&out.stderr).contains("PASS"), "{out:?}");
    let _ = fs::remove_dir_all(&dir);
}
