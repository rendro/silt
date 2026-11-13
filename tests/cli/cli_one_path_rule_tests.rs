//! One rule for the path given to `silt run`, `check`, `test`, `fmt` and
//! `disasm`: a `.silt` file or a directory, and a directory that holds a
//! `silt.toml` is its package. Any other file is refused, and so is a
//! directory that is no package where one program is needed (`run`,
//! `disasm`). `silt test` given a path in a package checks the package
//! first; `silt check` takes directories; a file given twice is checked
//! and shown once. FAIL on the base commit: `check` read `prog.txt` and
//! `silt.toml` as silt, `check`/`disasm` refused a package's directory,
//! `test prog.txt` passed with no test, `test <path>` skipped the
//! package check, and `check bad.silt ./bad.silt` showed the error twice.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A directory holding a package `pk` (whose `src/util.silt` has a type
/// error that no test file shows), a directory `plain` that is no
/// package, a `bad.silt` with a type error and a `prog.txt`.
fn workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("silt_one_path_{tag}_{}_{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("pk/src")).unwrap();
    fs::create_dir_all(dir.join("plain")).unwrap();
    fs::write(
        dir.join("pk/silt.toml"),
        "[package]\nname = \"pk\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("pk/src/main.silt"),
        "fn main() {\n  println(\"pk\")\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("pk/src/util.silt"),
        "pub fn v() -> Int {\n  let x: Int = \"s\"\n  x\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("pk/src/main_test.silt"),
        "import test\n\nfn test_one() {\n  test.assert_eq(1, 1)\n}\n",
    )
    .unwrap();
    fs::write(dir.join("plain/ok.silt"), "fn main() {\n  println(1)\n}\n").unwrap();
    fs::write(
        dir.join("bad.silt"),
        "fn main() {\n  let y: Int = \"t\"\n  println(y)\n}\n",
    )
    .unwrap();
    fs::write(dir.join("prog.txt"), "x\n").unwrap();
    dir
}

fn silt(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

/// What `out` wrote to stderr, with `/` for every path separator.
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).replace('\\', "/")
}

#[test]
fn a_file_that_is_not_silt_is_refused_by_every_command() {
    let dir = workspace("not_silt");
    for (command, takes) in [
        ("check", "a .silt file or a directory"),
        ("test", "a .silt file or a directory"),
        ("fmt", "a .silt file or a directory"),
        ("disasm", "a .silt file or a package directory"),
    ] {
        for file in ["prog.txt", "pk/silt.toml"] {
            let out = silt(&dir, &[command, file]);
            assert_eq!(out.status.code(), Some(1), "{command} {file}: {out:?}");
            assert_eq!(
                stderr(&out),
                format!("error: silt {command}: {file} is not {takes}\n"),
                "{command} {file}"
            );
            assert!(out.stdout.is_empty(), "{command} {file}: {out:?}");
        }
    }
    // `run` says too how to give the file to the program.
    let out = silt(&dir, &["run", "prog.txt"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        stderr(&out).starts_with("error: silt run: prog.txt is not a .silt file; "),
        "{out:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_directory_that_is_no_package_is_refused_where_one_program_is_needed() {
    let dir = workspace("plain_dir");
    for command in ["run", "disasm"] {
        let out = silt(&dir, &[command, "plain"]);
        assert_eq!(out.status.code(), Some(1), "{command}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!("error: silt {command}: plain is not a .silt file or a package directory\n")
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_package_directory_is_the_package_for_every_command() {
    let dir = workspace("package_dir");
    // `check` checks every module of the package, also one nothing
    // imports.
    let out = silt(&dir, &["check", "pk"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        stderr(&out).contains(" --> pk/src/util.silt:2:16\n"),
        "{out:?}"
    );
    // `disasm` disassembles the package's main.
    fs::remove_file(dir.join("pk/src/util.silt")).unwrap();
    let out = silt(&dir, &["disasm", "pk"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("main"),
        "{out:?}"
    );
    let out = silt(&dir, &["check", "pk"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let out = silt(&dir, &["test", "pk"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_test_command_given_a_path_in_a_package_checks_the_package_first() {
    let dir = workspace("test_path");
    for path in ["pk", "pk/src", "pk/src/main_test.silt"] {
        let out = silt(&dir, &["test", path]);
        assert_eq!(out.status.code(), Some(1), "{path}: {out:?}");
        let err = stderr(&out);
        assert!(
            err.contains(" --> pk/src/util.silt:2:16\n"),
            "{path}: {err}"
        );
        assert!(
            err.ends_with(
                "error: silt test: the package has errors (see above), so no test was run\n"
            ),
            "{path}: {err}"
        );
    }
    // Outside a package there is no package to check.
    let out = silt(&dir, &["test", "plain"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn check_takes_a_directory_that_is_no_package() {
    let dir = workspace("check_dir");
    fs::write(
        dir.join("plain/worse.silt"),
        "fn main() {\n  let z: Int = \"u\"\n  println(z)\n}\n",
    )
    .unwrap();
    let out = silt(&dir, &["check", "plain"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        stderr(&out).contains(" --> plain/worse.silt:2:16\n"),
        "{out:?}"
    );
    fs::create_dir_all(dir.join("empty")).unwrap();
    let out = silt(&dir, &["check", "empty"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        stderr(&out),
        "error: silt check: no .silt files found in empty\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_given_under_two_spellings_is_shown_once() {
    let dir = workspace("twice");
    let once = |out: &Output, at: &str| {
        let err = stderr(out);
        assert_eq!(err.matches("error[type]").count(), 1, "{err}");
        assert!(err.contains(at), "{err}");
    };
    let out = silt(&dir, &["check", "bad.silt", "./bad.silt"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    once(&out, " --> bad.silt:2:16\n");
    // A module of a package given beside the package.
    let out = silt(&dir, &["check", "pk", "./pk/src/util.silt"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    once(&out, " --> pk/src/util.silt:2:16\n");
    let out = silt(&dir, &["check", "pk", "pk/"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    once(&out, " --> pk/src/util.silt:2:16\n");
    let _ = fs::remove_dir_all(&dir);
}

/// A directory whose `.silt` files are all in packages below it: the walk
/// leaves them to their packages, and the error names one. FAILS on the
/// base: "no .silt files found in outer", though there are some.
#[test]
fn a_directory_whose_files_are_in_a_package_below_names_the_package() {
    let dir = workspace("nested");
    fs::create_dir_all(dir.join("outer")).unwrap();
    fs::rename(dir.join("pk"), dir.join("outer/pk")).unwrap();
    // A package that holds no .silt file is not named.
    fs::create_dir_all(dir.join("outer/bare")).unwrap();
    fs::write(
        dir.join("outer/bare/silt.toml"),
        "[package]\nname = \"bare\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (command, verb) in [("check", "check"), ("fmt", "format")] {
        let out = silt(&dir, &[command, "outer"]);
        assert_eq!(out.status.code(), Some(1), "{command}: {out:?}");
        assert_eq!(
            stderr(&out),
            format!(
                "error: silt {command}: no .silt files found in outer, but for those of the \
                 package in outer/pk: name its directory to {verb} it\n"
            ),
            "{command}"
        );
    }
    let out = silt(&dir.join("outer"), &["fmt"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        stderr(&out),
        "error: silt fmt: no .silt files found in current directory, but for those of the \
         package in pk: name its directory to format it\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A test file reached through a link from outside its package is run in
/// the package of the file it leads to, so that package is checked
/// first. FAILS on the base: the package of the link's directory (none)
/// was looked for, and the test ran over the package's error.
#[cfg(unix)]
#[test]
fn a_test_file_reached_through_a_link_checks_its_own_package_first() {
    let dir = workspace("test_link");
    std::os::unix::fs::symlink(dir.join("pk/src/main_test.silt"), dir.join("out_test.silt"))
        .unwrap();
    // And through a link to the package's src directory.
    std::os::unix::fs::symlink(dir.join("pk/src"), dir.join("srclink")).unwrap();
    for path in ["out_test.silt", "srclink/main_test.silt"] {
        let out = silt(&dir, &["test", path]);
        assert_eq!(out.status.code(), Some(1), "{path}: {out:?}");
        let err = stderr(&out);
        assert!(err.contains("pk/src/util.silt:2:16\n"), "{path}: {err}");
        assert!(
            err.ends_with(
                "error: silt test: the package has errors (see above), so no test was run\n"
            ),
            "{path}: {err}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}
