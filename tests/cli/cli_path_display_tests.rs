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
            "error reading no\\u{1b}csuch_test.silt: ",
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
/// argument a command does not take) is echoed by the display rule too;
/// a command silt suggests is not offered with such a word. FAIL on the
/// base: the words were echoed raw.
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
            "make the directory, go into it and run silt init there",
        ),
        (
            &["a\u{1b}cb.silt", "x\ty"],
            "To run the file, give it to 'silt run'.",
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

/// A command silt suggests for the user to paste holds a name only when
/// the name can be pasted as it stands (ASCII letters, digits, `_`, `-`,
/// `.`, `/`, not starting with `-`); otherwise the suggestion is said in
/// words. FAIL on the base: `mkdir my app && cd my app` was offered, and a
/// name like `x; rm -rf y` went into the command line as it stood.
#[test]
fn a_suggested_command_holds_a_name_only_when_it_can_be_pasted() {
    let dir = fixture(
        "paste",
        &[
            ("main.silt", "fn main() {\n  println(1)\n}\n"),
            ("notes.txt", "x"),
            ("my notes.txt", "x"),
        ],
    );
    let offered: [(&[&str], &str); 4] = [
        (
            &["init", "my_app"],
            "run: mkdir my_app && cd my_app && silt init",
        ),
        (
            &["hello.silt", "-w"],
            "Did you mean 'silt run hello.silt -w'?",
        ),
        (
            &["run", "main.silt", "extra"],
            "(e.g. 'silt run <file>.silt -- extra')",
        ),
        (&["run", "notes.txt"], "`silt run <file>.silt -- notes.txt`"),
    ];
    for (args, expected) in offered {
        let text = silt(&dir, args);
        assert!(
            text.contains(expected),
            "silt {args:?}: expected {expected:?} in:\n{text}"
        );
    }
    let in_words = "make the directory, go into it and run silt init there";
    let said: [(&[&str], &str, &str); 5] = [
        (&["init", "my app"], in_words, "mkdir"),
        (&["init", "x; rm -rf y"], in_words, "mkdir"),
        (
            &["my file.silt"],
            "To run the file, give it to 'silt run'.",
            "Did you mean",
        ),
        (
            &["run", "main.silt", "a b"],
            "If 'a b' is meant for the program, separate it with '--'.",
            "e.g.",
        ),
        (
            &["run", "my notes.txt"],
            "to give it to a program, write it after `--`",
            "-- my notes",
        ),
    ];
    for (args, expected, absent) in said {
        let text = silt(&dir, args);
        assert!(
            text.contains(expected),
            "silt {args:?}: expected {expected:?} in:\n{text}"
        );
        assert!(
            !text.contains(absent),
            "silt {args:?}: no command with the word:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The note of a test file run as a program names `silt test` with its
/// path only when the path can be pasted as it stands.
#[test]
fn the_test_file_note_names_the_path_only_when_it_can_be_pasted() {
    let dir = fixture(
        "testnote",
        &[
            ("a_test.silt", "fn test_a() {\n}\n"),
            ("my dir/a_test.silt", "fn test_a() {\n}\n"),
        ],
    );
    let text = silt(&dir, &["run", "a_test.silt"]);
    assert!(
        text.contains("run it with 'silt test a_test.silt' instead"),
        "{text}"
    );
    let text = silt(&dir, &["run", "my dir/a_test.silt"]);
    assert!(text.contains("run it with 'silt test' instead"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
