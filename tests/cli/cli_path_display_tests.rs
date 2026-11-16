//! A file or directory name with a control character is written by the
//! display rule in every line a command prints, as the place line of a
//! diagnostic (` --> `) writes it: `a\u{1b}cb.silt`. A name that held
//! `ESC c` used to reset the terminal of whoever ran `silt test` or
//! `silt fmt --check .` where it lay. Unix only: Windows holds no control
//! character in a file name.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

/// A fresh directory with `files` (name, text) in it.
fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("silt-path-display-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// What `silt args...` prints in `dir`, stdout and stderr together.
fn silt(dir: &PathBuf, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .output()
        .expect("silt runs");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// The lines of `text` that silt composed: all but the echoed source
/// line of a diagnostic (` 2 | ...`), which is the program's own text.
fn composed(text: &str) -> Vec<&str> {
    (text.lines())
        .filter(|line| {
            let t = line.trim_start();
            !(t.split_once(" |")
                .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit())))
        })
        .collect()
}

const UNFORMATTED: &str = "fn main() {\n      println(nope)\n}\n";
const FAILING_TEST: &str = "fn test_a() {\n  nope\n}\n";

/// FAIL on the base: `./a<ESC>cb.silt: not formatted`, raw.
#[test]
fn every_command_writes_a_file_name_with_a_control_character_escaped() {
    let dir = fixture(
        "commands",
        &[
            ("a\u{1b}cb.silt", UNFORMATTED),
            ("x\u{1b}c_test.silt", FAILING_TEST),
            ("d\u{7}e/main.silt", UNFORMATTED),
        ],
    );
    let runs: [(&[&str], &str); 7] = [
        (&["fmt", "--check", "."], "./a\\u{1b}cb.silt: not formatted"),
        (
            &["fmt", "--check", "a\u{1b}cb.silt"],
            "a\\u{1b}cb.silt: not formatted",
        ),
        (
            &["fmt", "--check", "d\u{7}e"],
            "d\\u{7}e/main.silt: not formatted",
        ),
        (&["test"], "./x\\u{1b}c_test.silt: failed to compile"),
        (
            &["check", "no\u{1b}csuch.silt"],
            "error reading no\\u{1b}csuch.silt: ",
        ),
        (
            &["run", "no\u{1b}csuch.silt"],
            "error reading no\\u{1b}csuch.silt: ",
        ),
        (
            &["test", "no\u{1b}csuch_test.silt"],
            "no\\u{1b}csuch_test.silt: failed to read",
        ),
    ];
    for (args, expected) in runs {
        let text = silt(&dir, args);
        assert!(
            text.contains(expected),
            "silt {args:?}: expected {expected:?} in:\n{text}"
        );
        for line in composed(&text) {
            assert!(
                !line.contains(['\u{1b}', '\u{7}']),
                "silt {args:?} printed a raw control character: {line:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file of a package named with one: the status line of `silt fmt`.
#[test]
fn a_package_file_named_with_a_control_character_is_escaped() {
    let dir = fixture(
        "package",
        &[
            (
                "silt.toml",
                "[package]\nname = \"pk\"\nversion = \"0.1.0\"\n",
            ),
            ("src/main.silt", "fn main() {\n  println(1)\n}\n"),
            ("src/m\u{202e}n.silt", "fn f() -> Int {\n      1\n}\n"),
        ],
    );
    let text = silt(&dir, &["fmt", "--check"]);
    assert!(
        text.contains("./src/m\\u{202e}n.silt: not formatted"),
        "{text}"
    );
    let text = silt(&dir, &["fmt"]);
    assert!(text.contains("./src/m\\u{202e}n.silt: formatted"), "{text}");
    assert!(!text.contains('\u{202e}'), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A typed word that is no path (an unknown command, an unknown flag, an
/// argument a command does not take) is echoed by the display rule too,
/// and so are the commands silt suggests with it. FAIL on the base: the
/// words were echoed raw.
#[test]
fn a_typed_word_with_a_control_character_is_escaped() {
    let dir = fixture("words", &[("main.silt", "fn main() {\n  println(1)\n}\n")]);
    let runs: [(&[&str], &str); 6] = [
        (&["zz\u{1b}c"], "unknown command 'zz\\u{1b}c'"),
        (&["check", "--x\u{1b}cy"], "unknown flag '--x\\u{1b}cy'"),
        (&["run", "-\t"], "unknown flag '-\\t'"),
        (&["repl", "x\u{7}y"], "unexpected argument 'x\\u{7}y'"),
        (
            &["init", "x\u{202e}y"],
            "mkdir x\\u{202e}y && cd x\\u{202e}y",
        ),
        (
            &["a\u{1b}cb.silt", "x\ty"],
            "Did you mean 'silt run \"a\\u{1b}cb.silt\" \"x\\ty\"'",
        ),
    ];
    for (args, expected) in runs {
        let text = silt(&dir, args);
        assert!(
            text.contains(expected),
            "silt {args:?}: expected {expected:?} in:\n{text}"
        );
        assert!(
            !text.contains(['\u{1b}', '\u{7}', '\t', '\u{202e}']),
            "silt {args:?} printed a raw character:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
