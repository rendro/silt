//! A print whose text goes in pieces (one longer than 1,000,000 bytes, to
//! the command's stdout) is a wait of its task, as any print into a full
//! pipe is: into a pipe nobody reads, a `task.deadline` around it ends
//! it, a cancel of its task returns, and other tasks run meanwhile
//! (review of #336, L1: it held its scheduler thread). Each program ends
//! by `process.exit`, whose code says what happened, while the test never
//! reads the pipe.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The code `program` exits with, run by the command with a stdout that
/// is never read; `None` if it has not ended after 30 seconds.
fn exit_code_unread(name: &str, program: &str) -> Option<i32> {
    let dir = std::env::temp_dir().join(format!("silt_pieces_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    std::fs::write(&file, program).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn silt");
    let started = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status.code();
        }
        if started.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // (The pipe is read only now, after the program has ended.)
    drop(child.stdout.take());
    let _ = std::fs::remove_dir_all(&dir);
    code
}

/// A value whose text is about 4 MB, named `a`.
const BIG: &str = "  let a = string.split(string.repeat(\"abcdefgh,\", 400000), \",\")\n";

#[test]
fn a_deadline_ends_a_print_in_pieces() {
    let program = format!(
        "import process\nimport string\nimport task\nimport time\nfn main() {{\n{BIG}  \
         let r = task.deadline(time.seconds(1)) {{ -> println(a) }}\n  match r {{\n    \
         Ok(_) -> process.exit(2)\n    Err(_) -> process.exit(3)\n  }}\n}}\n"
    );
    assert_eq!(exit_code_unread("deadline", &program), Some(3));
}

#[test]
fn a_cancel_of_a_task_in_a_print_in_pieces_returns() {
    let program = format!(
        "import process\nimport string\nimport task\nimport time\nfn main() {{\n{BIG}  \
         let t = task.spawn {{ -> println(a) }}\n  time.sleep(time.seconds(1))\n  \
         task.cancel(t)\n  process.exit(4)\n}}\n"
    );
    assert_eq!(exit_code_unread("cancel", &program), Some(4));
}

#[test]
fn other_tasks_run_while_eight_print_in_pieces() {
    let program = format!(
        "import list\nimport process\nimport string\nimport task\nimport time\nfn main() {{\n{BIG}  \
         let ts = list.map([1, 2, 3, 4, 5, 6, 7, 8]) {{ _ -> task.spawn {{ -> println(a) }} }}\n  \
         time.sleep(time.seconds(1))\n  let n = task.join(task.spawn {{ -> 40 + 2 }})\n  \
         let _ = ts\n  process.exit(n)\n}}\n"
    );
    assert_eq!(exit_code_unread("ninth", &program), Some(42));
}

/// A host's own output gets each print as one write, as docs/ffi.md
/// says, up to the length a string may have; a longer text is an error
/// of the print (review of #336, L2).
#[test]
fn a_hosts_output_gets_one_write_a_print() {
    use silt::session::{Builtins, Config, Entry, LockPolicy, ProjectSetup, Session};
    use silt::{HostIo, Output, Vm};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Writes(Arc<Mutex<Vec<usize>>>);
    impl Output for Writes {
        fn write(&self, text: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(text.len());
            Ok(())
        }
    }
    let run = |body: &str| {
        let mut session = Session::new(Config {
            project: ProjectSetup::None,
            lock: LockPolicy::ReadOnly,
            host: vec![],
            builtins: Builtins::All,
        });
        let source = format!("import string\nfn main() {{\n{BIG}  {body}\n}}\n");
        let file = session.set_overlay(Path::new("main.silt"), source);
        let program = session.compile(file, Entry::Main).expect("compiles");
        let out = Writes::default();
        let ran = Vm::new(HostIo::new(out.clone(), Writes::default())).run_program(&program);
        let writes = out.0.lock().unwrap().clone();
        (ran, writes)
    };
    // (`a` shows as 4,000,000 bytes and some; three of it are past the
    // string limit.)
    let (ran, writes) = run("println(a)");
    assert!(ran.is_ok(), "{ran:?}");
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert!(writes[0] > 4_000_000, "{writes:?}");
    let (ran, writes) = run("println([a, a, a])");
    let err = ran.expect_err("past the string limit");
    assert!(
        err.message
            .contains("println: result would exceed maximum string size"),
        "{}",
        err.message
    );
    assert!(writes.is_empty(), "{writes:?}");
}

/// A Regex shows its pattern in a print in pieces as in a print made
/// whole: a list of 300,001 of them (about 1.2 MB, past the 1,000,000
/// bytes printed whole) is written as the short list's text repeated, and
/// interpolation in pieces says the same.
#[test]
fn a_regex_in_a_print_in_pieces_shows_its_pattern() {
    let dir = std::env::temp_dir().join(format!("silt_pieces_{}_regex", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.silt");
    let program = "import list\nimport regex\nimport string\n\
        fn main() -> Result((), regex.RegexError) {\n  \
        let re = regex.compile(\"a+\")?\n  \
        let many = string.split(string.repeat(\"x,\", 300000), \",\") |> list.map { _ -> re }\n  \
        println([re, re])\n  println(many)\n  println(\"{many}\")\n  Ok(())\n}\n";
    std::fs::write(&file, program).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .output()
        .expect("run silt");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "three lines");
    assert_eq!(lines[0], "[a+, a+]");
    let long = format!("[{}a+]", "a+, ".repeat(300_000));
    assert!(long.len() > 1_000_000, "the print goes in pieces");
    assert!(
        lines[1] == long,
        "a print in pieces: {} bytes, not the short text's form",
        lines[1].len()
    );
    assert!(lines[2] == long, "interpolation: {} bytes", lines[2].len());
}
