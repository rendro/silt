//! A task of a REPL session sees what later entries define.
//!
//! A task that an entry starts goes on while the next entries are
//! typed. When a later entry hands it a closure, or a value, of its own
//! (through a channel), the task runs it as the session's own code
//! would: it finds the functions, `let`s, impls and traits that were
//! defined after it was started. It used to keep the view of the entry
//! it was started in: a later function was `the global '?' is read
//! before it is set`, a later impl `no method 'n' for type 'Int'`, a
//! later trait's method "a method the program does not have", and a
//! later `Display` impl was silently not used.
//!
//! Each case is run with a task that waits for its job when the job is
//! sent, with a task that never waits (it polls a buffered channel, so
//! nothing stands between getting the job and running it, and it runs
//! while the session takes the later entries in), and with three tasks
//! at once.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// A session that has not ended after this long is stopped, and its
/// test fails: none of them waits for anything that does not come.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// What `silt repl` wrote for the session `input`: its output after the
/// greeting, and its errors. The session must end by itself, with
/// success.
fn repl(input: &str) -> (String, String) {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "silt_repl_tasks_{}_{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a directory for the session");
    // (Files, not pipes: a session that is stopped leaves nobody
    // waiting to read.)
    let file = |name: &str| dir.join(name);
    std::fs::write(file("stdin"), input).expect("the session is written");
    let open = |name: &str| std::fs::File::create(file(name)).expect("a file of the session");
    let mut silt = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("repl")
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .env("SILT_HISTORY_FILE", file("history"))
        .stdin(Stdio::from(
            std::fs::File::open(file("stdin")).expect("the session is read"),
        ))
        .stdout(Stdio::from(open("stdout")))
        .stderr(Stdio::from(open("stderr")))
        .spawn()
        .expect("silt repl starts");
    let started = Instant::now();
    let status = loop {
        match silt.try_wait().expect("silt repl is waited for") {
            Some(status) => break Some(status),
            None if started.elapsed() >= RUN_TIMEOUT => {
                let _ = silt.kill();
                let _ = silt.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };
    let read = |name: &str| std::fs::read_to_string(file(name)).unwrap_or_default();
    let (stdout, stderr) = (read("stdout").replace("\r\n", "\n"), read("stderr"));
    let _ = std::fs::remove_dir_all(&dir);
    let status = status.unwrap_or_else(|| {
        panic!("the session did not end:\n{input}\nstdout:\n{stdout}\nstderr:\n{stderr}")
    });
    assert!(
        status.success(),
        "{status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let output = stdout
        .strip_prefix("Silt REPL (type :quit to exit, :help for commands)\n")
        .unwrap_or_else(|| panic!("the session's greeting: {stdout}"));
    (output.to_string(), stderr)
}

/// How the tasks that run a session's jobs get them.
#[derive(Clone, Copy, Debug)]
enum Workers {
    /// One task, which waits for its job.
    Waiting,
    /// One task, which asks for its job again and again and never
    /// waits: it runs while the later entries are taken in, and runs
    /// the job in the step after it got it.
    Polling,
    /// Three tasks, each of which waits for a job.
    Three,
}

impl Workers {
    fn count(self) -> usize {
        match self {
            Workers::Three => 3,
            _ => 1,
        }
    }
}

/// A session that declares `earlier`, starts its tasks, declares
/// `later`, and then has each task run the closure `{ -> job }` and
/// shows what each gave.
fn session(workers: Workers, earlier: &str, later: &str, job: &str) -> String {
    let work = match workers {
        Workers::Polling => {
            "fn work() {
  match channel.try_receive(jobs) {
    channel.Message(job) -> channel.send(outs, job())
    _ -> work()
  }
}"
        }
        _ => {
            "fn work() {
  match channel.receive(jobs) {
    channel.Message(job) -> channel.send(outs, job())
    _ -> ()
  }
}"
        }
    };
    let each = |line: &dyn Fn(usize) -> String| -> String {
        (0..workers.count())
            .map(line)
            .collect::<Vec<_>>()
            .join("\n")
    };
    [
        "import task\nimport channel".to_string(),
        "let jobs = channel.new(8)\nlet outs = channel.new(8)".to_string(),
        earlier.to_string(),
        work.to_string(),
        each(&|n| format!("let w{n} = task.spawn {{ -> work() }}")),
        later.to_string(),
        each(&|_| format!("channel.send(jobs, {{ -> {job} }})")),
        each(&|_| "channel.receive(outs)".to_string()),
        each(&|n| format!("task.join(w{n})")),
    ]
    .join("\n")
        + "\n"
}

/// The tasks, started after `earlier` and before `later`, each run
/// `job` and give `expected`; the session reports no error.
fn assert_tasks_see(earlier: &str, later: &str, job: &str, expected: &str) {
    for workers in [Workers::Waiting, Workers::Polling, Workers::Three] {
        let input = session(workers, earlier, later, job);
        let (output, errors) = repl(&input);
        assert_eq!(
            output,
            format!("Message({expected})\n").repeat(workers.count()),
            "{workers:?}\n{input}\nstderr:\n{errors}"
        );
        assert_eq!(errors, "", "{workers:?}\n{input}");
    }
}

/// It was `the global '?' is read before it is set`.
#[test]
fn a_task_calls_a_function_and_reads_a_let_of_a_later_entry() {
    assert_tasks_see(
        "",
        "fn later() -> String { \"fn\" }\nlet g = 5",
        "\"{later()} {g}\"",
        "fn 5",
    );
}

/// An impl for a builtin type, written after the task was started and
/// called on a value: the call names the impl's method.
#[test]
fn a_task_calls_a_method_of_an_impl_of_a_later_entry() {
    assert_tasks_see(
        "trait N { fn n(self) -> String }",
        "trait N for Int { fn n(self) -> String { \"int\" } }",
        "5.n()",
        "int",
    );
}

/// The same through a generic function of an entry before the task,
/// which finds the method by the type of the value it is given: of a
/// builtin type, and of a type of a later entry. It was `no method 'n'
/// for type 'Int'`.
#[test]
fn a_task_finds_an_impl_of_a_later_entry_through_an_earlier_generic_function() {
    assert_tasks_see(
        "trait N { fn n(self) -> String }\nfn call(v: a) -> String where a: N { v.n() }",
        "type Q { q: Int }\n\
         trait N for Int { fn n(self) -> String { \"int\" } }\n\
         trait N for Q { fn n(self) -> String { \"q{self.q}\" } }",
        "\"{call(5)} {call(Q { q: 1 })}\"",
        "int q1",
    );
}

/// A `Display` impl of a later entry is used wherever the value is
/// shown: in an interpolation and by `.display()`, inside a list. It
/// was not used, and nothing said so: `[Q {q: 2}]|[Q {q: 2}]`.
#[test]
fn a_task_shows_a_value_with_the_display_impl_of_a_later_entry() {
    assert_tasks_see(
        "fn sh(v: a) -> String where a: Display { \"{v}|{v.display()}\" }",
        "type Q { q: Int }\ntrait Display for Q { fn display(self) -> String { \"Q!\" } }",
        "sh([Q { q: 2 }])",
        "[Q!]|[Q!]",
    );
}

/// A trait of a later entry, its method called on a value whose type
/// the call does not know. It was `the call of a method names a method
/// the program does not have`.
#[test]
fn a_task_calls_a_method_of_a_trait_of_a_later_entry() {
    assert_tasks_see(
        "",
        "trait M { fn m(self) -> String }\ntrait M for Int { fn m(self) -> String { \"m{self}\" } }",
        "{\n  let call = { y -> y.m() }\n  call(7)\n}",
        "m7",
    );
}

/// Not a closure but a value of a later entry's type, shown by the
/// task's own code, which is of an entry before the type and its
/// `Display` impl.
#[test]
fn a_task_shows_a_value_of_a_later_entry_in_its_own_code() {
    let (output, errors) = repl(
        "import task
import channel
let values = channel.new(8)
let outs = channel.new(8)
fn show() {
  match channel.receive(values) {
    channel.Message(v) -> channel.send(outs, \"shown: {v}\")
    _ -> ()
  }
}
let shower = task.spawn { -> show() }
type Q { q: Int }
trait Display for Q { fn display(self) -> String { \"Q!\" } }
channel.send(values, [Q { q: 3 }])
channel.receive(outs)
task.join(shower)
",
    );
    assert_eq!(
        (output.as_str(), errors.as_str()),
        ("Message(shown: [Q!])\n", "")
    );
}

/// An entry that fails adds nothing a task can see: one that does not
/// check declares nothing (the function it would have replaced is the
/// one the task calls), and what an entry declared before it failed
/// when it ran is gone with it, for the task as for the session.
#[test]
fn an_entry_that_fails_adds_nothing_a_task_sees() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
let outs = channel.new(8)
fn f() -> Int { 1 }
fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      channel.Message(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      _ -> ()
    }
  }
}
let worker = task.spawn { -> work(3) }
fn f() -> Int { \"not an Int\" }
channel.send(jobs, { -> f() })
channel.receive(outs)
let f = 1 / 0
channel.send(jobs, { -> f() + 10 })
channel.receive(outs)
fn f() -> Int { 2 }
channel.send(jobs, { -> f() + 20 })
channel.receive(outs)
task.join(worker)
",
    );
    assert_eq!(output, "Message(1)\nMessage(11)\nMessage(22)\n", "{errors}");
    let errors: Vec<&str> = (errors.lines())
        .filter(|line| line.starts_with("error"))
        .collect();
    assert_eq!(
        errors,
        [
            "error[type]: type mismatch: expected Int, got String",
            "error[runtime]: division by zero"
        ]
    );
}

/// A session only ever adds to what its tasks can find: a second impl
/// of a trait for a type is refused in a later entry as it is in one
/// file, so a method that a task has found once is the method for
/// good. (`Vm::look_again` rests on it: a task looks for the session's
/// newest definitions only where it has found none.)
#[test]
fn a_second_impl_in_a_later_entry_is_refused() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
let outs = channel.new(8)
trait N { fn n(self) -> String }
trait N for Int { fn n(self) -> String { \"first\" } }
fn work(left) {
  match left {
    0 -> ()
    _ -> match channel.receive(jobs) {
      channel.Message(job) -> {
        channel.send(outs, job())
        work(left - 1)
      }
      _ -> ()
    }
  }
}
let worker = task.spawn { -> work(2) }
channel.send(jobs, { -> 5.n() })
channel.receive(outs)
trait N for Int { fn n(self) -> String { \"second\" } }
5.n()
channel.send(jobs, { -> 5.n() })
channel.receive(outs)
task.join(worker)
",
    );
    assert_eq!(
        output, "Message(first)\nfirst\nMessage(first)\n",
        "{errors}"
    );
    let errors: Vec<&str> = (errors.lines())
        .filter(|line| line.starts_with("error"))
        .collect();
    assert_eq!(
        errors,
        ["error[type]: duplicate implementation of trait 'N' for type 'Int'"]
    );
}

/// The session ends while a task still runs code of a later entry
/// (it never waits) and another still waits: it ends at once, and
/// neither is reported.
#[test]
fn the_session_ends_while_a_task_runs_code_of_a_later_entry() {
    let (output, errors) = repl(
        "import task
import channel
let jobs = channel.new(8)
fn spin(n) {
  match channel.try_receive(jobs) {
    channel.Message(job) -> spin(job())
    _ -> spin(n)
  }
}
let spinning = task.spawn { -> spin(0) }
let waiting = task.spawn { -> channel.receive(channel.new(0)) }
fn later() -> Int { 7 }
channel.send(jobs, { -> later() })
channel.send(jobs, { -> later() + 1 })
1 + 1
",
    );
    assert_eq!((output.as_str(), errors.as_str()), ("2\n", ""));
}
