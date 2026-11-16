//! What an embedder prints of an error is what the `silt` command prints
//! (audit R9, card EMB-1): `Session::render` of each diagnostic is a
//! part of what `silt check` writes for the same file, and
//! `VmError::render` is what `silt run` writes for the error that ended
//! the program, its place and call stack included, and the one line of
//! a `main` that returned `Err`. Both are plain text, whatever the
//! process's stderr and environment say (card EMB-2): they are compared
//! with what the command prints without colour, and a child process run
//! with `FORCE_COLOR=1` shows them without a colour code.

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
        let rendered = session.render(d);
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
        let rendered = e.render(&session);
        assert_eq!(printed.trim_end(), rendered.trim_end(), "{name}");
    }
}

/// The variable that makes `print_both_renders` print, in the child
/// process `renders_are_plain_under_force_color` starts.
const CHILD: &str = "SILT_EMBEDDER_RENDER_CHILD";

/// In the child only: print a rendered diagnostic and a rendered runtime
/// error (the test process's own environment is left as it is).
#[test]
fn print_both_renders() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    let (mut session, file) = session_of("fn main() {\n  let x: Int = \"no\"\n  println(x)\n}\n");
    let d = session.analyze(file).diagnostics[0].clone();
    println!("{}", session.render(&d));
    let (mut session, file) = session_of("fn main() {\n  let z = 0\n  println(1 / z)\n}\n");
    let program = session.compile(file, Entry::Main).expect("compiles");
    let out = silt::Buffer::new();
    let e = silt::Vm::new(silt::HostIo::buffer(&out))
        .run_program(&program)
        .expect_err("divides by zero");
    println!("{}", e.render(&session));
}

/// `FORCE_COLOR=1` colours what the command prints, and not what the two
/// render functions give: the test binary runs `print_both_renders` in a
/// child process with that environment.
#[test]
fn renders_are_plain_under_force_color() {
    let dir = fixture(
        "force-color",
        "fn main() {\n  let x: Int = \"no\"\n  println(x)\n}\n",
    );
    let command = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(["check", "main.silt"])
        .current_dir(&dir)
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .output()
        .expect("silt runs");
    let command = String::from_utf8_lossy(&command.stderr).to_string();
    assert!(
        command.contains('\x1b'),
        "the command colours under FORCE_COLOR:\n{command}"
    );

    let child = Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "embedder_render_tests::print_both_renders",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("FORCE_COLOR", "1")
        .env_remove("NO_COLOR")
        .output()
        .expect("the test binary runs");
    let printed = String::from_utf8_lossy(&child.stdout).to_string();
    assert!(
        child.status.success(),
        "{printed}\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(
        printed.contains("error[type]: type mismatch: expected Int, got String")
            && printed.contains("error[runtime]: division by zero"),
        "the child printed both renders:\n{printed}"
    );
    assert!(
        !printed.contains('\x1b'),
        "a render has a colour code:\n{printed}"
    );
}
