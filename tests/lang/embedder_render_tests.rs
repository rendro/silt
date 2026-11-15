//! What an embedder prints of an error is what the `silt` command prints
//! (audit R9, card EMB-1): `Session::render` of each diagnostic is a
//! part of what `silt check` writes for the same file, and
//! `VmError::render` is what `silt run` writes for the error that ended
//! the program, its place and call stack included, and the one line of
//! a `main` that returned `Err`.

use std::path::{Path, PathBuf};
use std::process::Command;

use silt::session::{Builtins, Config, Entry, LockPolicy, ProjectSetup, Session};

/// A directory of its own for `name`, with `main.silt` holding `text`.
fn fixture(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "silt-embedder-render-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("the fixture directory is made");
    std::fs::write(dir.join("main.silt"), text).expect("main.silt is written");
    dir
}

/// What the command writes on stderr for `silt <command> main.silt` in `dir`.
fn command_stderr(dir: &Path, command: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(command)
        .arg("main.silt")
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .output()
        .expect("silt runs");
    assert!(!out.status.success(), "silt {command} fails on the fixture");
    String::from_utf8(out.stderr).expect("stderr is UTF-8")
}

/// `text` without ANSI colour codes (the test process's environment
/// decides whether the library colours).
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn session_of(text: &str) -> (Session, silt::session::FileId) {
    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![],
        builtins: Builtins::All,
    });
    let file = session.set_overlay(Path::new("main.silt"), text.to_string());
    (session, file)
}

#[test]
fn a_rendered_diagnostic_is_what_the_check_command_prints() {
    let text =
        "import list\n\nfn main() {\n  let x: Int = \"no\"\n  let y = 1 + true\n  println(x)\n}\n";
    let dir = fixture("check", text);
    let printed = command_stderr(&dir, "check");
    let (mut session, file) = session_of(text);
    let diagnostics = session.analyze(file).diagnostics.clone();
    assert!(diagnostics.len() >= 3, "{diagnostics:?}");
    for d in &diagnostics {
        let rendered = plain(&session.render(d));
        assert!(
            printed.contains(&rendered),
            "`silt check` does not print the rendered diagnostic\n{rendered}\nin\n{printed}"
        );
    }
}

/// The error of `main` (with a call stack of two frames), and the line
/// of a `main` that returned `Err`.
#[test]
fn a_rendered_runtime_error_is_what_the_run_command_prints() {
    let cases = [
        (
            "raised",
            "fn half(z: Int) -> Int {\n  10 / z\n}\n\nfn main() {\n  println(half(0))\n}\n",
        ),
        (
            "returned",
            "fn main() -> Result((), String) {\n  Err(\"no input\")\n}\n",
        ),
    ];
    for (name, text) in cases {
        let dir = fixture(name, text);
        let printed = command_stderr(&dir, "run");
        let (mut session, file) = session_of(text);
        let program = session.compile(file, Entry::Main).expect("compiles");
        let out = silt::Buffer::new();
        let e = silt::Vm::new(silt::HostIo::buffer(&out))
            .run_program(&program)
            .expect_err("the program fails");
        let rendered = plain(&e.render(&session));
        assert_eq!(printed.trim_end(), rendered.trim_end(), "{name}");
    }
}
