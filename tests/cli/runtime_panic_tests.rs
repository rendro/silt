//! What the `silt` binary does when a thread of its runtime panics: it
//! ends, with Rust's own lines for the panic (the thread, the place,
//! the message), one line that says it is a bug in silt, and exit
//! status 101. Before, a panic on a worker, the timer's thread or a
//! thread of the I/O pool left the process hanging: the tasks that
//! waited for what the lost thread was doing, and `main` with them,
//! waited for ever.
//!
//! The process ends where the panic is raised, in a panic hook of the
//! command's own: the three go together (Rust's lines, the line, the
//! status), the line is the last of stderr, and a stderr that is held
//! up does not keep the process alive.
//!
//! No silt program can make the runtime panic. A build with the
//! `test-hooks` feature reads `SILT_TEST_PANIC` in `silt run` and makes
//! the runtime panic once at the place it names; every other build
//! ignores the variable, which the last test here shows for a build
//! without the feature.
//!
//! No test asserts a time. A process that does not end is killed after
//! a minute so that the test fails instead of hanging the run.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A run of the `silt` command in a directory of its own.
struct Run<'a> {
    name: &'a str,
    /// The files of the directory.
    files: &'a [(&'a str, &'a str)],
    args: &'a [&'a str],
    /// Variables beside the ones every run has.
    env: &'a [(&'a str, &'a str)],
    /// Stdout is a pipe that nobody reads while the process runs: a
    /// thread that writes enough to it is blocked in its write.
    unread_stdout: bool,
}

impl Run<'_> {
    fn dir(&self) -> PathBuf {
        let name = format!("silt_runtime_panic_{}_{}", std::process::id(), self.name);
        std::env::temp_dir().join(name)
    }

    /// The command, in its directory with its files.
    fn command(&self) -> Command {
        let dir = self.dir();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a directory for the program");
        for (file, text) in self.files {
            fs::write(dir.join(file), text).expect("a file is written");
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_silt"));
        command
            .args(self.args)
            .current_dir(&dir)
            .env("NO_COLOR", "1")
            .env_remove("FORCE_COLOR")
            .env_remove("RUST_BACKTRACE")
            .env_remove("SILT_TEST_PANIC")
            .env_remove("SILT_TIME_SLICE")
            .envs(self.env.iter().copied())
            .stdin(Stdio::null())
            .stderr(fs::File::create(dir.join("err")).expect("a file for stderr"));
        match self.unread_stdout {
            true => command.stdout(Stdio::piped()),
            false => command.stdout(fs::File::create(dir.join("out")).expect("a file for stdout")),
        };
        command
    }

    /// Wait for `child`, which the command started: its status and
    /// what it wrote to stderr. A process that has not ended after a
    /// minute is killed, and the test fails.
    fn wait(&self, mut child: Child) -> (Option<i32>, String) {
        let started = Instant::now();
        let status = loop {
            match child.try_wait().expect("the process is asked") {
                Some(status) => break status,
                None if started.elapsed() >= Duration::from_secs(60) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let written =
                        |file: &str| fs::read_to_string(self.dir().join(file)).unwrap_or_default();
                    panic!(
                        "silt did not end: it hangs ({})\nstderr: {}\nstdout: {}",
                        self.name,
                        written("err"),
                        written("out")
                    );
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        };
        let stderr = fs::read_to_string(self.dir().join("err")).expect("stderr is read");
        (status.code(), stderr)
    }

    /// The status, and what the process wrote to stdout (nothing is
    /// read of a pipe) and to stderr.
    fn ended(&self) -> (Option<i32>, String, String) {
        let child = self.command().spawn().expect("silt starts");
        let (status, stderr) = self.wait(child);
        let stdout = match self.unread_stdout {
            true => String::new(),
            false => fs::read_to_string(self.dir().join("out")).expect("stdout is read"),
        };
        let _ = fs::remove_dir_all(self.dir());
        (status, stdout, stderr)
    }
}

/// What `silt run` of `source` wrote and how it ended, with
/// `SILT_TEST_PANIC` set to `place` if there is one.
fn run(name: &str, source: &str, place: Option<&str>) -> (Option<i32>, String, String) {
    let env: Vec<(&str, &str)> = place
        .iter()
        .map(|place| ("SILT_TEST_PANIC", *place))
        .collect();
    Run {
        name,
        files: &[("main.silt", source)],
        args: &["run", "main.silt"],
        env: &env,
        unread_stdout: false,
    }
    .ended()
}

/// The line that says a panic with `message` is a bug in silt.
#[cfg(feature = "test-hooks")]
fn internal_error_line(message: &str) -> String {
    format!(
        "silt: internal error: {message}. This is a bug in silt, not in your program: please \
         report it."
    )
}

/// `<handle:0>` would send on a channel that nobody else sends on; a
/// task joins it, a task receives on that channel, and `main` joins
/// both.
///
/// The hook loses a task before a slice of it that is not its first,
/// so the task must certainly have one. A receive alone does not make
/// it so: it may find its value there and go on in the same slice. A
/// sleep does: it always ends the slice. So the task is let through
/// the gate by `main`, when `main` has started every task and printed
/// its line (nothing is spawned or printed while the runtime may be
/// broken), and then sleeps; it is lost as that sleep ends. That the
/// joiner, the receiver and `main` wait by then is likely and not
/// certain: the program ends either way.
const LOST_WHILE_ALL_WAIT: &str = r#"import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(5))
    channel.send(only, 1)
  }
  let joiner = task.spawn { -> task.join(lost) }
  let receiver = task.spawn { -> channel.receive(only) }
  println("main waits")
  channel.send(gate, 1)
  task.join(joiner)
  let _ = task.join(receiver)
  println("main went on")
}
"#;

/// The process ended as silt ends after a panic of `thread` made at
/// `place`: status 101; what `main` printed before; Rust's lines for
/// the panic, and then, as the last line, the one that calls it a bug.
#[cfg(feature = "test-hooks")]
fn assert_internal_error(ended: (Option<i32>, String, String), thread: &str, place: &str) {
    let (status, stdout, stderr) = ended;
    assert_eq!(status, Some(101), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "main waits\n", "stderr: {stderr}");
    let message = format!("a panic made for a test: {place}");
    let rust = format!("thread '{thread}' ");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines.iter().position(|line| line.starts_with(&rust));
    let at = at.unwrap_or_else(|| panic!("no line of Rust's for the panic in: {stderr}"));
    assert!(lines[at].contains(" panicked at src"), "{stderr}");
    assert_eq!(lines[at + 1], message, "{stderr}");
    assert_eq!(
        lines.last().copied(),
        Some(internal_error_line(&message).as_str()),
        "{stderr}"
    );
    assert!(
        !stderr.contains("error["),
        "no error of the program: {stderr}"
    );
}

/// A worker panics as it is about to run a task that a joiner, a
/// receiver and `main` all wait on.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_a_worker_ends_the_program() {
    let ended = run("worker", LOST_WHILE_ALL_WAIT, Some("task:0"));
    assert_internal_error(ended, "silt-worker", "Task(0)");
}

/// The timer's thread panics when the first deadline is due, while a
/// task sleeps and `main` joins it. The task is let through a gate when
/// `main` has printed its line, which the process could otherwise end
/// before (as it did on a busy machine with the sleep below, which is
/// over at once): see `LOST_WHILE_ALL_WAIT`. The same for the I/O pool.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_the_timer_thread_ends_the_program() {
    let source = r#"import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let sleeper = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(5))
  }
  println("main waits")
  channel.send(gate, 1)
  task.join(sleeper)
  println("main went on")
}
"#;
    let ended = run("timer", source, Some("timer"));
    assert_internal_error(ended, "silt-timer", "Timer");
}

/// The same with a sleep that is over before its wait begins: its
/// deadline goes through the timer all the same. (A worker held up for
/// longer than the sleep between the call and the wait did the same to
/// a sleep of 5 ms: the wait ended at once, the timer never ran, and
/// the program went on.)
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_the_timer_thread_ends_the_program_after_a_sleep_over_at_once() {
    let source = r#"import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let sleeper = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.nanos(1))
  }
  println("main waits")
  channel.send(gate, 1)
  task.join(sleeper)
  println("main went on")
}
"#;
    let ended = run("timer_at_once", source, Some("timer"));
    assert_internal_error(ended, "silt-timer", "Timer");
}

/// A thread of the I/O pool panics when its operation has returned,
/// before it hands the value to the task that waits for it, which
/// `main` joins.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_an_io_thread_ends_the_program() {
    let source = r#"import channel
import io
import task

fn main() {
  let gate = channel.new(0)
  let reader = task.spawn { ->
    let _ = channel.receive(gate)
    io.read_file("main.silt")
  }
  println("main waits")
  channel.send(gate, 1)
  let _ = task.join(reader)
  println("main went on")
}
"#;
    let ended = run("io", source, Some("io"));
    assert_internal_error(ended, "silt-io", "Io");
}

/// The thread that runs `main` panics in the VM's code while a task
/// waits on a channel: the process ended before this step too, and now
/// says what it is.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_the_program_thread_ends_the_program() {
    let source = r#"import channel
import task
import time

fn main() {
  let never = channel.new(0)
  let waits = task.spawn { -> channel.receive(never) }
  println("main waits")
  let _ = channel.receive_timeout(never, time.ms(1))
  println("main went on")
}
"#;
    let ended = run("program", source, Some("program"));
    assert_internal_error(ended, "silt-main", "Program");
}

/// `main` is not where the scheduler could wake it when the worker
/// panics: it sleeps on the clock for an hour, it computes without end,
/// it polls for what the lost task would have sent. The process ends
/// all the same, at once: the thread that caught the panic ends it.
/// And what `main` had printed without a line end is not lost. (The
/// task is let through the gate when `main` has printed, and sleeps
/// before it is lost: see `LOST_WHILE_ALL_WAIT`.)
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_a_worker_ends_a_program_whose_main_thread_is_busy() {
    let lost = r#"import channel
import task
import time

fn forever(n) {
  forever(n + 1)
}

fn poll(only) {
  match channel.try_receive(only) {
    channel.Message(_) -> ()
    _ -> poll(only)
  }
}

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let _lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(1))
    channel.send(only, 1)
  }
  println("main waits")
  print("and has printed this")
  channel.send(gate, 1)
  BUSY
  println("main went on")
}
"#;
    let busy = [
        ("sleeps", "time.sleep(time.seconds(3600))"),
        ("computes", "forever(0)"),
        ("polls", "poll(only)"),
    ];
    for (name, busy) in busy {
        let (status, stdout, stderr) = run(name, &lost.replace("BUSY", busy), Some("task:0"));
        assert_eq!(
            stdout, "main waits\nand has printed this",
            "{name}: {stderr}"
        );
        let ended = (status, "main waits\n".to_string(), stderr);
        assert_internal_error(ended, "silt-worker", "Task(0)");
    }
}

/// A thread of the program is blocked in a write to stdout, a pipe
/// that nobody reads, and holds stdout's lock when a thread of the
/// runtime panics. The process ends all the same: the thread that ends
/// it waits for the flush of stdout for a moment and no longer. The
/// writer is `main` when the timer's thread panics; a task, with
/// `main` asleep, when a worker does; a task when `main`'s own thread
/// does.
#[test]
#[cfg(feature = "test-hooks")]
fn a_writer_blocked_in_stdout_does_not_keep_the_process_alive() {
    let flood = r#"import channel
import task
import time

fn flood(n) {
  println("a line of some length to fill the pipe with, number {n}")
  flood(n + 1)
}

fn main() {
  MAIN
}
"#;
    let writer_is_main = "let sleeper = task.spawn { -> time.sleep(time.ms(300)) }\n  flood(0)";
    let writer_is_a_task = "let sleeper = task.spawn { -> time.sleep(time.ms(300)) }\n  \
                            let writer = task.spawn { -> flood(0) }\n  \
                            time.sleep(time.seconds(3600))";
    let main_panics = "let never = channel.new(0)\n  \
                       let writer = task.spawn { -> flood(0) }\n  \
                       let _ = channel.receive_timeout(never, time.ms(300))";
    let cases = [
        ("flood_main", writer_is_main, "timer", "silt-timer", "Timer"),
        (
            "flood_task",
            writer_is_a_task,
            "task:0",
            "silt-worker",
            "Task(0)",
        ),
        (
            "flood_program",
            main_panics,
            "program",
            "silt-main",
            "Program",
        ),
    ];
    for (name, main, place, thread, at) in cases {
        let (status, _, stderr) = Run {
            name,
            files: &[("main.silt", &flood.replace("MAIN", main))],
            args: &["run", "main.silt"],
            env: &[("SILT_TEST_PANIC", place)],
            unread_stdout: true,
        }
        .ended();
        assert_eq!(status, Some(101), "{name}: {stderr}");
        assert!(
            stderr.contains(&format!("thread '{thread}' ")),
            "{name}: {stderr}"
        );
        let line = internal_error_line(&format!("a panic made for a test: {at}"));
        assert_eq!(stderr.lines().last(), Some(line.as_str()), "{name}");
    }
}

/// The line and the status go together. `main` ends with an error of
/// its own, or returns `Err`, without waiting for its tasks, just as a
/// worker panics: two threads set out to end the process. Whichever
/// comes first ends it: with 101, Rust's lines for the panic and the
/// line, or with the program's status 1 and none of them. Never 1 with
/// the line, and never 1 with Rust's lines: the process's end is
/// decided where the panic is raised, before Rust writes them. (It was
/// decided at the top of the panicking thread, after they were
/// written and the thread had unwound, and `main` could end the
/// process in between.) Half of the runs are made with a backtrace,
/// which takes Rust a while to write.
///
/// And the line is the last of stderr: `main`'s own report stands
/// whole before Rust's lines or is not written. (The command looked
/// whether the process was ending and then wrote; a panic between the
/// two put the report after the line, in about one run of 200.)
#[test]
#[cfg(feature = "test-hooks")]
fn the_internal_error_line_and_the_status_101_go_together() {
    let tasks = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/concurrency/tasks"
    );
    for program in [
        "stage7_c5__a_program_whose_main_fails_does_not_wait",
        "stage7_c5__a_main_that_returns_err_does_not_wait",
    ] {
        let source = fs::read_to_string(format!("{tasks}/{program}.silt")).expect("the golden");
        for round in 0..20 {
            let backtrace = ["0", "1"][round % 2];
            let (status, _, stderr) = Run {
                name: "status",
                files: &[("main.silt", &source)],
                args: &["run", "main.silt"],
                env: &[
                    ("SILT_TEST_PANIC", "task:0"),
                    ("SILT_TIME_SLICE", "1"),
                    ("RUST_BACKTRACE", backtrace),
                ],
                unread_stdout: false,
            }
            .ended();
            let line = stderr
                .lines()
                .any(|line| line.starts_with("silt: internal error: "));
            assert_eq!(
                status == Some(101),
                line,
                "{program}, round {round}: {stderr}"
            );
            let rusts = stderr.lines().any(|line| line.contains(" panicked at "));
            assert_eq!(rusts, line, "{program}, round {round}: {stderr}");
            let last = stderr.lines().last().unwrap_or_default();
            assert_eq!(
                last.starts_with("silt: internal error: "),
                line,
                "{program}, round {round}: {stderr}"
            );
            assert!(
                matches!(status, Some(1 | 101)),
                "{program}, round {round}: {status:?}"
            );
        }
    }
}

/// Nothing of the command's follows the internal-error line. `main`
/// is spawning twenty tasks when a worker panics: a spawn that comes
/// after it is refused by the broken runtime, and `main` ends with
/// that refusal. It is not the program's error and is not printed as
/// one: the line is the last on stderr, in every one of forty runs at
/// a slice of one step, where the two meet most often. (Before, the
/// runtime was marked as broken before the process's end was decided,
/// and in some runs `error[runtime]: cannot spawn a task: ...` stood
/// after the line.)
#[test]
#[cfg(feature = "test-hooks")]
fn no_error_of_the_program_follows_the_internal_error_line() {
    let source = r#"import channel
import task
import time
import list

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    channel.send(only, 1)
  }
  let waker = task.spawn { ->
    time.sleep(time.ms(10))
    channel.send(gate, 1)
  }
  print("main waits")
  let js = list.map([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20]) { _ -> task.spawn { -> task.join(lost) } }
  list.each(js) { j -> task.join(j) }
  println("main went on")
}
"#;
    let line = internal_error_line("a panic made for a test: Task(0)");
    // (At a slice of one step the task is due again after its first
    // instruction: it is lost there, before it comes to any wait.)
    for round in 0..40 {
        let (status, stdout, stderr) = Run {
            name: "joiners",
            files: &[("main.silt", source)],
            args: &["run", "main.silt"],
            env: &[("SILT_TEST_PANIC", "task:0"), ("SILT_TIME_SLICE", "1")],
            unread_stdout: false,
        }
        .ended();
        assert_eq!(status, Some(101), "round {round}: {stderr}");
        assert!(!stdout.contains("main went on"), "round {round}: {stdout}");
        let lines = stderr
            .lines()
            .filter(|l| l.starts_with("silt: internal error: "));
        assert_eq!(lines.count(), 1, "round {round}: {stderr}");
        assert_eq!(
            stderr.lines().last(),
            Some(line.as_str()),
            "round {round}: {stderr}"
        );
        assert!(!stderr.contains("error["), "round {round}: {stderr}");
    }
}

/// Two workers panic close together: Rust's lines and the
/// internal-error line of the panic that came first, and status 101.
/// (Each task is let through the gate by `main` and then sleeps, so
/// each is certainly lost if it is still to run when its sleep ends;
/// the two sleeps end within microseconds of each other. Whether both
/// workers panic or the second finds the process ended is the
/// machine's to say.)
///
/// The panic that comes second writes nothing: Rust's lines are there
/// once, for the panic the line names, and the line is the last of
/// stderr. (Rust wrote its lines for the second panic too, and they
/// could stand after the line.)
#[test]
#[cfg(feature = "test-hooks")]
fn two_panics_close_together_are_one_internal_error() {
    let source = r#"import channel
import task
import time

fn nap(gate) {
  let _ = channel.receive(gate)
  time.sleep(time.ms(1))
}

fn main() {
  let gate = channel.new(0)
  let first = task.spawn { -> nap(gate) }
  let second = task.spawn { -> nap(gate) }
  println("main waits")
  channel.send(gate, 1)
  channel.send(gate, 2)
  task.join(first)
  task.join(second)
  println("main went on")
}
"#;
    for round in 0..10 {
        let (status, stdout, stderr) = run("two", source, Some("task:0,task:1"));
        assert_eq!(status, Some(101), "round {round}: {stderr}");
        assert_eq!(stdout, "main waits\n", "round {round}: {stderr}");
        let lines: Vec<&str> = stderr
            .lines()
            .filter(|line| line.starts_with("silt: internal error: "))
            .collect();
        let either = [
            internal_error_line("a panic made for a test: Task(0)"),
            internal_error_line("a panic made for a test: Task(1)"),
        ];
        assert_eq!(lines.len(), 1, "round {round}: {stderr}");
        assert!(
            either.iter().any(|line| line == lines[0]),
            "round {round}: {stderr}"
        );
        assert!(!stderr.contains("error["), "round {round}: {stderr}");
        assert_eq!(stderr.lines().last(), Some(lines[0]), "round {round}");
        let rusts: Vec<&str> = stderr
            .lines()
            .filter(|line| line.contains(" panicked at "))
            .collect();
        assert_eq!(rusts.len(), 1, "round {round}: {stderr}");
        let message = lines[0]
            .strip_prefix("silt: internal error: ")
            .and_then(|rest| rest.split_once(". This is a bug"))
            .map(|(message, _)| message);
        let named = stderr.lines().skip_while(|line| *line != rusts[0]).nth(1);
        assert_eq!(named, message, "round {round}: {stderr}");
    }
}

/// A pipe that is filled to the last byte: its write end takes no
/// more, and a write to it waits. The read end, which nobody reads,
/// comes first.
#[cfg(all(unix, feature = "test-hooks"))]
fn full_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let (unread, full) = {
        let mut ends = [0; 2];
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0, "a pipe");
        unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) }
    };
    let flags = unsafe { libc::fcntl(full.as_raw_fd(), libc::F_GETFL) };
    let set = |flags: libc::c_int| unsafe { libc::fcntl(full.as_raw_fd(), libc::F_SETFL, flags) };
    assert_eq!(set(flags | libc::O_NONBLOCK), 0, "the pipe does not block");
    let byte = *b".";
    while unsafe { libc::write(full.as_raw_fd(), byte.as_ptr().cast(), 1) } == 1 {}
    assert_eq!(set(flags), 0, "the pipe blocks again");
    (unread, full)
}

/// Stderr is a pipe that is full and that nobody reads when a worker
/// panics, and `main` waits for the task that is lost: Rust cannot
/// write its lines for the panic. The process ends all the same, with
/// status 101, when it has waited for them for ten seconds. (The
/// panicking thread stayed in that write for ever, the panic never
/// reached the top of its thread, and the process hung.)
#[test]
#[cfg(all(unix, feature = "test-hooks"))]
fn a_stderr_that_is_held_up_does_not_keep_the_process_alive() {
    let (unread, full) = full_pipe();
    let run = Run {
        name: "stderr",
        files: &[("main.silt", LOST_WHILE_ALL_WAIT)],
        args: &["run", "main.silt"],
        env: &[("SILT_TEST_PANIC", "task:0")],
        unread_stdout: false,
    };
    let mut command = run.command();
    command.stderr(Stdio::from(full));
    let child = command.spawn().expect("silt starts");
    // The command holds the write end no longer: only the process does.
    drop(command);
    let (status, _) = run.wait(child);
    drop(unread);
    assert_eq!(status, Some(101));
}

/// Stdout and stderr are both held up, each a full pipe that nobody
/// reads, and `main` has printed a text without a line end, which
/// waits in stdout's buffer, when a worker panics. The process ends
/// with status 101: neither Rust's lines nor the line nor the flush
/// of that text is waited for without a bound, and the exit does not
/// flush stdout itself. (The exit did, when the thread that had
/// waited ten seconds for Rust's lines made it, and the process
/// hung.)
#[test]
#[cfg(all(unix, feature = "test-hooks"))]
fn a_stdout_and_a_stderr_that_are_held_up_do_not_keep_the_process_alive() {
    let source = r#"import channel
import task
import time

fn main() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let _lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(5))
    channel.send(only, 1)
  }
  print("a line without its end")
  channel.send(gate, 1)
  let _ = channel.receive(only)
  println("main went on")
}
"#;
    let (unread_out, full_out) = full_pipe();
    let (unread_err, full_err) = full_pipe();
    let run = Run {
        name: "both",
        files: &[("main.silt", source)],
        args: &["run", "main.silt"],
        env: &[("SILT_TEST_PANIC", "task:0")],
        unread_stdout: false,
    };
    let mut command = run.command();
    command
        .stdout(Stdio::from(full_out))
        .stderr(Stdio::from(full_err));
    let child = command.spawn().expect("silt starts");
    drop(command);
    let (status, _) = run.wait(child);
    drop((unread_out, unread_err));
    assert_eq!(status, Some(101));
}

/// The reader of stdout takes one line and closes, while `main`
/// prints without end and a worker panics. A process whose reader is
/// gone dies by the signal of the closed pipe (or ends with its status,
/// 141), quietly; one that is
/// ending with the internal error does not: it ends with Rust's
/// lines, the line and status 101. Never Rust's lines and a death by
/// the signal. (From the moment the process's end is decided, the
/// closed stdout is an error of the write. Before, the signal could
/// kill the process between Rust's lines and the line, or after the
/// line.)
#[test]
#[cfg(all(unix, feature = "test-hooks"))]
fn a_closed_stdout_does_not_kill_a_process_that_ends_with_the_internal_error() {
    use std::io::{BufRead, BufReader};
    let source = r#"import task
import time

fn flood(n) {
  println("a line of some length to fill the pipe with, number {n}")
  flood(n + 1)
}

fn tick() {
  time.sleep(time.ms(1))
  tick()
}

fn main() {
  let _ticker = task.spawn { -> tick() }
  flood(0)
}
"#;
    for round in 0..30 {
        let run = Run {
            name: "closed",
            files: &[("main.silt", source)],
            args: &["run", "main.silt"],
            env: &[("SILT_TEST_PANIC", "task:0")],
            unread_stdout: true,
        };
        let mut child = run.command().spawn().expect("silt starts");
        let mut reader = BufReader::new(child.stdout.take().expect("a pipe"));
        let mut first = String::new();
        reader.read_line(&mut first).expect("a line is read");
        drop(reader);
        let (status, stderr) = run.wait(child);
        let rusts = stderr.lines().any(|line| line.contains(" panicked at "));
        let line = stderr
            .lines()
            .any(|line| line.starts_with("silt: internal error: "));
        match status {
            // Ended by the closed pipe: the reader was gone first. It
            // dies by the signal, or (where the signal is the process's
            // and lands on another thread, as on macOS) the write that
            // met the closed pipe ends it with the status 141 of that
            // death (`ProcessStdout`).
            None | Some(141) => assert!(!rusts && !line, "round {round}: {stderr}"),
            status => {
                assert_eq!(status, Some(101), "round {round}: {stderr}");
                assert!(rusts && line, "round {round}: {stderr}");
            }
        }
    }
}

/// A panic inside the call of a builtin is no panic of the runtime: it
/// is the error of the call, in the command's process as in an
/// embedder's. Rust writes its lines, the program ends with that
/// error and status 1, and no internal-error line is written. (The
/// hook that ends the process for a panic of silt's own code leaves
/// this one to the catch around the call.)
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_inside_a_builtin_is_the_error_of_the_call() {
    let source = r#"fn main() {
  println("the call that panics")
  println("main went on")
}
"#;
    let (status, stdout, stderr) = run("builtin", source, Some("builtin"));
    assert_eq!(status, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "", "stderr: {stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("thread 'silt-main' "))
        .unwrap_or_else(|| panic!("no line of Rust's for the panic in: {stderr}"));
    assert!(lines[at].contains(" panicked at src"), "{stderr}");
    assert_eq!(
        lines[at + 1],
        "a panic made for a test: Builtin",
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "error[runtime]: builtin module 'println' panicked: a panic made for a test: Builtin"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("silt: internal error"), "{stderr}");
}

/// The same on a worker: a task's call of a builtin panics. The task
/// fails with the call's error, `main` gets it from the join, and
/// the program ends with it and status 1.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_inside_a_builtin_of_a_task_is_the_error_of_the_task() {
    let source = r#"import int
import task

fn main() {
  let worker = task.spawn { -> int.abs(-1) }
  println("main waits")
  println(task.join(worker))
  println("main went on")
}
"#;
    let (status, stdout, stderr) = run("builtin_of_a_task", source, Some("builtin-of-a-task"));
    assert_eq!(status, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "main waits\n", "stderr: {stderr}");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("thread 'silt-worker' "))
        .unwrap_or_else(|| panic!("no line of Rust's for the panic in: {stderr}"));
    assert_eq!(
        lines[at + 1],
        "a panic made for a test: BuiltinOfATask",
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "error[runtime]: joined task failed: builtin module 'int' panicked: a panic made \
             for a test: BuiltinOfATask"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("silt: internal error"), "{stderr}");
}

/// After a panic that was the error of a call, a panic of the runtime
/// still ends the process at once: the catch around the call is
/// around nothing any more. `silt test`: the first test fails with
/// the builtin's error, the second passes, the third loses a worker,
/// and the run ends there with the line and status 101.
#[test]
#[cfg(feature = "test-hooks")]
fn after_a_caught_panic_a_panic_of_the_runtime_still_ends_the_process() {
    let source = r#"import channel
import task
import time

fn test_a_builtin_panics() {
  println("a: the call that panics")
  println("a went on")
}

fn test_b_passes() {
  println("b")
}

fn test_c_loses_a_worker() {
  let gate = channel.new(0)
  let only = channel.new(0)
  let lost = task.spawn { ->
    let _ = channel.receive(gate)
    time.sleep(time.ms(2))
    channel.send(only, 1)
  }
  let joiner = task.spawn { -> task.join(lost) }
  let receiver = task.spawn { -> channel.receive(only) }
  println("waits")
  channel.send(gate, 1)
  task.join(joiner)
  let _ = task.join(receiver)
  println("went on")
}
"#;
    let (status, stdout, stderr) = Run {
        name: "count",
        files: &[("caught_test.silt", source)],
        args: &["test", "caught_test.silt"],
        env: &[("SILT_TEST_PANIC", "builtin,task:0")],
        unread_stdout: false,
    }
    .ended();
    assert_eq!(status, Some(101), "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "b\nwaits\n", "stderr: {stderr}");
    assert!(
        stderr.contains("  FAIL caught_test.silt::test_a_builtin_panics"),
        "{stderr}"
    );
    assert!(
        stderr.contains("builtin module 'println' panicked: a panic made for a test: Builtin"),
        "{stderr}"
    );
    assert!(
        stderr.contains("  PASS caught_test.silt::test_b_passes"),
        "{stderr}"
    );
    let line = internal_error_line("a panic made for a test: Task(0)");
    assert_eq!(stderr.lines().last(), Some(line.as_str()), "{stderr}");
}

/// `silt test`: the second test of a file loses a worker. The run ends
/// there with the internal error; the first test's result was written.
#[test]
#[cfg(feature = "test-hooks")]
fn a_panic_of_a_worker_ends_a_test_run() {
    let tests = LOST_WHILE_ALL_WAIT.replace(
        "fn main() {",
        "fn test_a_passes() {\n  println(\"the first test runs\")\n}\n\nfn test_b_loses_a_worker() {",
    );
    let (status, stdout, stderr) = Run {
        name: "test",
        files: &[("lost_test.silt", &tests)],
        args: &["test", "lost_test.silt"],
        env: &[("SILT_TEST_PANIC", "task:0")],
        unread_stdout: false,
    }
    .ended();
    assert_eq!(status, Some(101), "{stderr}");
    assert_eq!(stdout, "the first test runs\nmain waits\n", "{stderr}");
    assert!(
        stderr.contains("PASS lost_test.silt::test_a_passes"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("lost_test.silt::test_b_loses_a_worker"),
        "{stderr}"
    );
    assert!(stderr.contains("thread 'silt-worker' "), "{stderr}");
    let line = internal_error_line("a panic made for a test: Task(0)");
    assert_eq!(stderr.lines().last(), Some(line.as_str()));
}

/// A thread of a runtime panics after its VM is gone, in silt's own
/// process: the line and 101, like any other. `silt test` makes a VM
/// for each file. The first file's test leaves a read of a FIFO behind
/// that nobody waits for; its VM is dropped; while the second file's
/// test sleeps, this test lets the read return, and its thread panics
/// with nobody left to give the panic to. Before, Rust's lines were
/// all, and the run went on to its usual end.
///
/// The order is certain, with no sleep to make it so: the FIFO can be
/// opened for writing only when the read has it open, so the read is
/// under way when this test makes the file `go`, which the first
/// file's test waits for before it cancels its reader.
#[test]
#[cfg(all(unix, feature = "test-hooks"))]
fn a_panic_after_its_vm_is_gone_ends_the_process() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::OpenOptionsExt;

    let first = r#"import fs
import io
import task
import time

fn until_there(path) {
  match fs.exists(path) {
    true -> ()
    false -> {
      time.sleep(time.ms(1))
      until_there(path)
    }
  }
}

fn test_a_leaves_a_read_behind() {
  let reader = task.spawn { -> io.read_file("pipe") }
  until_there("go")
  task.cancel(reader)
}
"#;
    let second = r#"import time

fn test_b_goes_on() {
  println("the second file runs")
  time.sleep(time.seconds(20))
}
"#;
    let run = Run {
        name: "late",
        files: &[("a_test.silt", first), ("b_test.silt", second)],
        args: &["test"],
        env: &[("SILT_TEST_PANIC", "io")],
        unread_stdout: true,
    };
    let mut command = run.command();
    let pipe = run.dir().join("pipe");
    let made = Command::new("mkfifo").arg(&pipe).status();
    assert!(
        made.is_ok_and(|status| status.success()),
        "mkfifo makes the FIFO"
    );
    let mut child = command.spawn().expect("silt starts");

    // The read has the FIFO open when a writer can open it (until
    // then the open fails, and is tried again). The writer is kept:
    // the read waits for what it writes, or for its end.
    let started = Instant::now();
    let mut options = fs::OpenOptions::new();
    options.write(true).custom_flags(libc::O_NONBLOCK);
    let writer = loop {
        match options.open(&pipe) {
            Ok(writer) => break writer,
            Err(_) if started.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("nobody reads the FIFO: {e}"),
        }
    };
    // The test may cancel its reader now, and end.
    fs::write(run.dir().join("go"), "").expect("the file is made");
    // The first file's VM is gone when the second file's test runs.
    let mut stdout = BufReader::new(child.stdout.take().expect("the child's stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("a line of the child");
    assert_eq!(line, "the second file runs\n");
    // The read that was left behind returns, and its thread panics.
    drop(writer);

    let (status, stderr) = run.wait(child);
    let _ = fs::remove_dir_all(run.dir());
    assert_eq!(status, Some(101), "{stderr}");
    assert!(stderr.contains("thread 'silt-io' "), "{stderr}");
    assert!(
        stderr.contains("PASS ./a_test.silt::test_a_leaves_a_read_behind"),
        "{stderr}"
    );
    let line = internal_error_line("a panic made for a test: Io");
    assert_eq!(stderr.lines().last(), Some(line.as_str()));
}

/// Without the variable the program of the first test runs to its end.
#[test]
fn the_program_runs_to_its_end_when_nothing_panics() {
    let (status, stdout, stderr) = run("plain", LOST_WHILE_ALL_WAIT, None);
    assert_eq!(stderr, "");
    assert_eq!(stdout, "main waits\nmain went on\n");
    assert_eq!(status, Some(0));
}

/// A build without `test-hooks` does not read the variable: the
/// program runs to its end with it set.
#[test]
#[cfg(not(feature = "test-hooks"))]
fn a_build_without_the_test_hooks_ignores_the_variable() {
    let (status, stdout, stderr) = run("ignored", LOST_WHILE_ALL_WAIT, Some("task:0"));
    assert_eq!(stderr, "");
    assert_eq!(stdout, "main waits\nmain went on\n");
    assert_eq!(status, Some(0));
}
