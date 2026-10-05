//! The embedder's output and clock (`silt::HostIo`): where a program's
//! `print`, `println` and the runtime's reports go, and which clock
//! `time.*`, the timeouts and the deadlines read.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use silt::session::testing::compile_str;
use silt::{Buffer, Clock, HostIo, Output, Value, Vm};

/// Run `source` on a VM with `io`, and give `main`'s value or the
/// message of the runtime error.
fn run(source: &str, io: HostIo) -> Result<Value, String> {
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    Vm::new(io).run_program(&program).map_err(|e| e.message)
}

/// [`run`] on a thread of its own, which ends with the program: the
/// runtime reports the task failures that nobody joined when the thread
/// that ran the program ends.
fn run_on_thread(source: &'static str, io: HostIo) -> thread::JoinHandle<Result<Value, String>> {
    thread::spawn(move || run(source, io))
}

/// Wait until `done` holds, for at most ten seconds.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let give_up = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < give_up, "timed out waiting until {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

// ── Output ──────────────────────────────────────────────────────────

#[test]
fn print_and_println_write_to_the_buffer() {
    let out = Buffer::new();
    let source =
        "fn main() {\n  print(\"a\")\n  print(\"b\")\n  println(\"c\")\n  println(1 + 1)\n}";
    assert_eq!(run(source, HostIo::buffer(&out)), Ok(Value::Unit));
    assert_eq!(out.contents(), "abc\n2\n");
    // `take` gives the text and empties the buffer.
    assert_eq!(out.take(), "abc\n2\n");
    assert_eq!(out.contents(), "");
}

#[test]
fn output_of_spawned_tasks_goes_to_the_buffer() {
    let out = Buffer::new();
    let source = r#"
import list
import task
fn main() {
  let handles = [1, 2, 3, 4] |> list.map({ n -> task.spawn({ ->
    println("task {n}")
    n
  }) })
  let total = handles |> list.fold(0, { acc, h -> acc + task.join(h) })
  println("total {total}")
}
"#;
    assert_eq!(run(source, HostIo::buffer(&out)), Ok(Value::Unit));
    let text = out.contents();
    let mut lines: Vec<&str> = text.lines().collect();
    // The tasks run in any order; main's line comes after their joins.
    assert_eq!(lines.pop(), Some("total 10"), "{text:?}");
    lines.sort();
    assert_eq!(lines, ["task 1", "task 2", "task 3", "task 4"], "{text:?}");
}

/// The report of a task that failed and that nobody joined is the
/// runtime's, not the program's: it goes to stderr.
#[test]
fn unjoined_task_failure_is_reported_on_the_stderr_buffer() {
    let out = Buffer::new();
    let err = Buffer::new();
    let source = r#"
import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(1)
  let _ = task.spawn({ ->
    channel.send(about_to_fail, 1)
    panic("boom")
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  println("main done")
}
"#;
    let ran = run_on_thread(source, HostIo::new(out.clone(), err.clone()));
    assert_eq!(ran.join().unwrap(), Ok(Value::Unit));
    assert_eq!(out.contents(), "main done\n");
    let report = err.contents();
    assert!(
        report.contains("task <handle:0> failed and was never joined: panic: boom"),
        "{report:?}"
    );
    assert!(report.contains("task.join"), "{report:?}");
}

/// An output that refuses every write.
struct Closed;

impl Output for Closed {
    fn write(&self, _text: &str) -> io::Result<()> {
        Err(io::Error::other("the sink is closed"))
    }
}

#[test]
fn a_failed_write_to_stdout_is_a_runtime_error() {
    let source = "fn main() {\n  println(\"hello\")\n  1\n}";
    let result = run(source, HostIo::new(Closed, Buffer::new()));
    assert_eq!(
        result,
        Err("cannot write to stdout: the sink is closed".to_string())
    );
}

/// The `Output` example of docs/ffi.md, as written there: one `write`
/// for each `print` and `println`.
#[test]
fn docs_ffi_output_of_ones_own() {
    struct Lines(std::sync::mpsc::Sender<String>);

    impl Output for Lines {
        fn write(&self, text: &str) -> std::io::Result<()> {
            self.0.send(text.to_string()).map_err(std::io::Error::other)
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let source = "fn main() {\n  print(\"a\")\n  println(\"b\")\n}";
    assert_eq!(
        run(source, HostIo::new(Lines(tx), Buffer::new())),
        Ok(Value::Unit)
    );
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), ["a", "b\n"]);
}

// ── Clock ───────────────────────────────────────────────────────────

/// 2026-10-05T12:00:00Z, in milliseconds since the Unix epoch.
const NOON_MS: u64 = 1_791_201_600_000;

/// A clock that stands still unless it is moved: by `advance`, or by a
/// `sleep` on it, which moves it by the duration and returns at once.
#[derive(Clone, Default)]
struct FakeClock {
    passed_ms: Arc<AtomicU64>,
}

impl FakeClock {
    fn advance(&self, duration: Duration) {
        self.passed_ms
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }

    fn passed(&self) -> Duration {
        Duration::from_millis(self.passed_ms.load(Ordering::SeqCst))
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(NOON_MS) + self.passed()
    }

    fn monotonic(&self) -> Duration {
        self.passed()
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// Move `clock` on by a minute every millisecond until the returned
/// guard is dropped: an hour on it takes about 60 ms.
fn keep_advancing(clock: &FakeClock) -> impl Drop {
    struct Stop(Arc<AtomicBool>, Option<thread::JoinHandle<()>>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            let _ = self.1.take().map(thread::JoinHandle::join);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let (clock, stopped) = (clock.clone(), stop.clone());
    let ticker = thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            clock.advance(Duration::from_secs(60));
            thread::sleep(Duration::from_millis(1));
        }
    });
    Stop(stop, Some(ticker))
}

#[test]
fn time_now_reads_the_clock() {
    let out = Buffer::new();
    let source = r#"
import time
fn main() {
  let dt = time.now() |> time.to_utc
  println(time.format(dt, "%Y-%m-%d %H:%M:%S"))
}
"#;
    let io = HostIo::buffer(&out).clock(FakeClock::default());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(out.contents(), "2026-10-05 12:00:00\n");
}

#[test]
fn time_today_is_the_date_of_the_clock() {
    let out = Buffer::new();
    let source = "import time\nfn main() { println(time.today()) }";
    let io = HostIo::buffer(&out).clock(FakeClock::default());
    assert_eq!(run(source, io), Ok(Value::Unit));
    // With the `local-clock` feature the date is the local one, which
    // is the day before or after in time zones far from UTC.
    #[cfg(feature = "local-clock")]
    let expected = {
        use chrono::TimeZone;
        chrono::Local
            .timestamp_opt((NOON_MS / 1000) as i64, 0)
            .unwrap()
            .date_naive()
            .to_string()
    };
    #[cfg(not(feature = "local-clock"))]
    let expected = "2026-10-05".to_string();
    assert_eq!(out.contents(), format!("{expected}\n"));
}

/// A `time.sleep` outside a task is the clock's `sleep`: on the fake
/// clock an hour passes at once.
#[test]
fn sleep_on_the_main_thread_is_the_clocks_sleep() {
    let out = Buffer::new();
    let clock = FakeClock::default();
    let source = r#"
import time
fn main() {
  let before = time.now()
  time.sleep(time.hours(1))
  println(time.since(before, time.now()) == time.hours(1))
}
"#;
    let started = Instant::now();
    let io = HostIo::buffer(&out).clock(clock.clone());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(out.contents(), "true\n");
    assert_eq!(clock.passed(), Duration::from_secs(3600));
    assert!(started.elapsed() < Duration::from_secs(60));
}

/// A task's `time.sleep` ends when the clock reaches its deadline.
#[test]
fn sleep_in_a_task_follows_the_clock() {
    let out = Buffer::new();
    let clock = FakeClock::default();
    let source = r#"
import task
import time
fn main() {
  let before = time.now()
  let h = task.spawn({ ->
    time.sleep(time.hours(1))
    time.since(before, time.now()) >= time.hours(1)
  })
  println(task.join(h))
}
"#;
    let _ticking = keep_advancing(&clock);
    let io = HostIo::buffer(&out).clock(clock.clone());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(out.contents(), "true\n");
}

/// `channel.timeout` and `channel.recv_timeout` end when the clock
/// reaches their deadline, on the main thread and in a task.
#[test]
fn channel_timeouts_follow_the_clock() {
    let out = Buffer::new();
    let clock = FakeClock::default();
    let source = r#"
import channel
import task
import time
fn main() {
  let quiet = channel.new(1)
  channel.send(quiet, 0)
  let _ = channel.receive(quiet)
  println(channel.receive(channel.timeout(3600000)))
  println(channel.recv_timeout(quiet, time.hours(1)))
  let h = task.spawn({ -> channel.recv_timeout(quiet, time.hours(1)) })
  println(task.join(h))
}
"#;
    let _ticking = keep_advancing(&clock);
    let io = HostIo::buffer(&out).clock(clock.clone());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(
        out.contents(),
        "Closed\nErr(channel receive timed out)\nErr(channel receive timed out)\n"
    );
    assert!(clock.passed() >= Duration::from_secs(3 * 3600));
}

/// A timeout waits for the clock, not for real time: while the clock
/// stands still, a 20 ms timeout does not end.
#[test]
fn a_timeout_does_not_end_while_the_clock_stands_still() {
    let out = Buffer::new();
    let clock = FakeClock::default();
    let source = r#"
import channel
fn main() {
  println("waiting")
  println(channel.receive(channel.timeout(20)))
}
"#;
    let ran = run_on_thread(source, HostIo::buffer(&out).clock(clock.clone()));
    wait_until("the program waits", || out.contents() == "waiting\n");
    thread::sleep(Duration::from_millis(300));
    assert!(!ran.is_finished(), "the timeout ended in real time");
    assert_eq!(out.contents(), "waiting\n");

    clock.advance(Duration::from_millis(20));
    assert_eq!(ran.join().unwrap(), Ok(Value::Unit));
    assert_eq!(out.contents(), "waiting\nClosed\n");
}

/// The deadline of `task.deadline` is a reading of the clock: once the
/// clock has passed it, I/O in the scope fails with the timeout.
#[test]
fn task_deadline_is_measured_on_the_clock() {
    let out = Buffer::new();
    let source = r#"
import io
import task
import time
fn main() {
  let early = task.deadline(time.hours(1), { -> io.read_file("/no/such/file") })
  println(early)
  let late = task.deadline(time.hours(1), { ->
    time.sleep(time.hours(2))
    io.read_file("/no/such/file")
  })
  println(late)
}
"#;
    let io = HostIo::buffer(&out).clock(FakeClock::default());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(
        out.contents(),
        "Err(file not found: /no/such/file)\nErr(I/O timeout (task.deadline exceeded))\n"
    );
}

/// The deadline of a task parked on I/O is the clock's too: the
/// scheduler's watchdog cancels the wait when the clock passes it.
#[cfg(feature = "tcp")]
#[test]
fn a_parked_tasks_deadline_follows_the_clock() {
    let out = Buffer::new();
    let clock = FakeClock::default();
    // Nobody connects: the accept waits until the deadline cancels it.
    let source = r#"
import task
import tcp
import time
fn main() {
  let h = task.spawn_until(time.hours(1), { ->
    match tcp.listen("127.0.0.1:0") {
      Ok(listener) -> match tcp.accept(listener) {
        Ok(_) -> "accepted"
        Err(e) -> "{e}"
      }
      Err(e) -> "cannot listen: {e}"
    }
  })
  println(task.join(h))
}
"#;
    let _ticking = keep_advancing(&clock);
    let io = HostIo::buffer(&out).clock(clock.clone());
    assert_eq!(run(source, io), Ok(Value::Unit));
    assert_eq!(out.contents(), "tcp operation timed out\n");
    assert!(clock.passed() >= Duration::from_secs(3600));
}

// ── docs/ffi.md ─────────────────────────────────────────────────────

/// The "Output and clock" example of docs/ffi.md, as written there.
#[test]
fn docs_ffi_output_and_clock() {
    use std::path::Path;
    use std::time::Duration;

    use silt::session::{Config, Entry, LockPolicy, ProjectSetup, Session};
    use silt::{Buffer, Clock, HostIo, Vm};

    // A clock that starts at a fixed time and moves only when the
    // program sleeps.
    struct Simulated(std::sync::Mutex<Duration>);

    impl Clock for Simulated {
        fn now(&self) -> Duration {
            // 2026-10-05T12:00:00Z, plus what has passed.
            Duration::from_secs(1_791_201_600) + self.monotonic()
        }
        fn monotonic(&self) -> Duration {
            *self.0.lock().unwrap()
        }
        fn sleep(&self, duration: Duration) {
            *self.0.lock().unwrap() += duration;
        }
    }

    let mut session = Session::new(Config {
        project: ProjectSetup::None,
        lock: LockPolicy::ReadOnly,
        host: vec![],
    });
    let source = r#"
import time
fn main() {
  println("started")
  time.sleep(time.minutes(90))
  println(time.now() |> time.to_utc |> time.format("%H:%M"))
}
"#;
    let file = session.set_overlay(Path::new("main.silt"), source.to_string());
    let program = session.compile(file, Entry::Main).expect("compiles");

    // The program's output is collected in `out`; it reads `Simulated`.
    let out = Buffer::new();
    let io = HostIo::buffer(&out).clock(Simulated(Default::default()));
    Vm::new(io).run_program(&program).unwrap();
    assert_eq!(out.contents(), "started\n13:30\n");
}
