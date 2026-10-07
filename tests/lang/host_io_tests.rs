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

/// [`run`] on a thread of its own, for a program the test watches
/// while it runs.
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

/// How long a test waits, in turn, for a task to fail after the task
/// has said it is about to: nothing shows the test that the scheduler
/// has recorded the failure, so an attempt that was too short on a busy
/// machine is repeated with the next, longer wait.
const WAITS_FOR_A_FAILURE_MS: [u64; 4] = [50, 500, 3_000, 10_000];

/// The report of a task that failed and that nobody joined is the
/// runtime's, not the program's: it goes to stderr. It is there when
/// `run_program` returns, whose result it does not change, if the task
/// has failed by then.
#[test]
fn unjoined_task_failure_is_reported_on_the_stderr_buffer() {
    for wait_ms in WAITS_FOR_A_FAILURE_MS {
        let out = Buffer::new();
        let err = Buffer::new();
        let source = format!(
            r#"
import channel
import task
import time
fn main() {{
  let about_to_fail = channel.new(1)
  let _ = task.spawn({{ ->
    channel.send(about_to_fail, 1)
    panic("boom")
  }})
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms({wait_ms}))
  println("main done")
}}
"#
        );
        let program = compile_str(&source).unwrap_or_else(|errors| panic!("{errors:?}"));
        let mut vm = Vm::new(HostIo::new(out.clone(), err.clone()));
        assert_eq!(
            vm.run_program(&program).map_err(|e| e.message),
            Ok(Value::Unit)
        );
        assert_eq!(out.contents(), "main done\n");
        let at_return = err.take();
        drop(vm);
        let at_drop = err.take();
        // Each failure is reported once: when the program returns, or,
        // if the task had not failed by then, when the VM is dropped.
        let report = format!("{at_return}{at_drop}");
        assert_eq!(report.matches("never joined").count(), 1, "{report:?}");
        assert!(
            report.contains("task <handle:0> failed and was never joined: panic: boom"),
            "{report:?}"
        );
        assert!(report.contains("task.join"), "{report:?}");
        if !at_return.is_empty() {
            return;
        }
    }
    panic!("the task's failure was never there when run_program returned");
}

/// `settle` waits until the program has ended: until its tasks can do
/// no more. A task that fails after `run_program` has returned has
/// failed by then, and is reported; a task that waits for something
/// nobody will give is dropped, and does not keep `settle` waiting.
#[test]
fn settle_waits_for_the_program_to_end_and_reports_what_failed() {
    let source = r#"
import channel
import task
import time
fn main() {
  let never = channel.new(0)
  let _waits = task.spawn({ -> channel.receive(never) })
  let _late = task.spawn({ ->
    time.sleep(time.ms(20))
    println("about to fail")
    panic("late")
  })
}
"#;
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    let out = Buffer::new();
    let err = Buffer::new();
    let mut vm = Vm::new(HostIo::new(out.clone(), err.clone()));
    assert_eq!(
        vm.run_program(&program).map_err(|e| e.message),
        Ok(Value::Unit)
    );
    vm.settle();
    assert_eq!(out.contents(), "about to fail\n");
    let report = err.contents();
    assert!(
        report.contains("task <handle:1> failed and was never joined: panic: late"),
        "the report: {report:?}"
    );
    // Nothing is left to report when the VM is dropped.
    drop(vm);
    assert_eq!(err.contents(), report);
}

/// The owner tag is the VM's. Two VMs on two threads, each with a tag
/// of its own, do not see each other's: `settle` on one waits for its
/// own sleeping task, whatever the other's tag is at that moment.
#[test]
fn the_owner_of_tasks_is_the_vms_own() {
    let source = r#"
import channel
import list
import task
import time

fn main() {
  let never = channel.new(0)
  let _parked = 1..3 |> list.map { _ -> task.spawn { -> channel.receive(never) } }
  -- Printed before the task exists: the order is not a matter of timing.
  println("main")
  let _sleeps = task.spawn { ->
    time.sleep(time.ms(1))
    println("slept")
  }
}
"#;
    let program = Arc::new(compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}")));
    let threads: Vec<_> = (1..=2u64)
        .map(|tag| {
            let program = program.clone();
            thread::spawn(move || {
                for run in 0..100 {
                    let out = Buffer::new();
                    let mut vm = Vm::new(HostIo::buffer(&out));
                    vm.set_task_owner(tag);
                    let result = vm.run_program(&program).map_err(|e| e.message);
                    vm.settle();
                    assert_eq!(result, Ok(Value::Unit), "tag {tag}, run {run}");
                    assert_eq!(out.contents(), "main\nslept\n", "tag {tag}, run {run}");
                }
            })
        })
        .collect();
    for thread in threads {
        thread
            .join()
            .expect("every settle waited for its own VM's tasks");
    }
}

/// A task that fails after `run_program` has returned is reported when
/// the VM is dropped, if it has failed by then.
#[test]
fn a_later_task_failure_is_reported_when_the_vm_is_dropped() {
    // The task sleeps on a clock that stands still: it cannot fail
    // before the test moves the clock.
    let source = r#"
import task
import time
fn main() {
  let _ = task.spawn({ ->
    time.sleep(time.hours(1))
    println("about to fail")
    panic("late")
  })
}
"#;
    let program = compile_str(source).unwrap_or_else(|errors| panic!("{errors:?}"));
    for wait_ms in WAITS_FOR_A_FAILURE_MS {
        let out = Buffer::new();
        let err = Buffer::new();
        let clock = FakeClock::default();
        let mut vm = Vm::new(HostIo::new(out.clone(), err.clone()).clock(clock.clone()));
        assert_eq!(
            vm.run_program(&program).map_err(|e| e.message),
            Ok(Value::Unit)
        );
        assert_eq!(err.contents(), "");

        // The task may not have started its sleep yet: the clock is
        // moved on until it has slept.
        let ticking = keep_advancing(&clock);
        wait_until("the task wakes", || out.contents() == "about to fail\n");
        drop(ticking);
        thread::sleep(Duration::from_millis(wait_ms));
        drop(vm);
        let report = err.contents();
        if report.is_empty() {
            // The VM was dropped before the task had failed.
            continue;
        }
        assert!(
            report.contains("failed and was never joined: panic: late"),
            "{report:?}"
        );
        return;
    }
    panic!("the task's failure was never reported when the VM was dropped");
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

/// An output that panics on every write.
struct Panics;

impl Output for Panics {
    fn write(&self, _text: &str) -> io::Result<()> {
        panic!("the sink panicked")
    }
}

/// A panic of the output is a runtime error of the `print` that hit
/// it, on the main thread and in a task: the scheduler's worker lives
/// on and the join returns.
#[test]
fn a_panicking_stdout_is_a_runtime_error() {
    let source = "fn main() {\n  println(\"x\")\n}";
    assert_eq!(
        run(source, HostIo::new(Panics, Buffer::new())),
        Err("cannot write to stdout: the output panicked: the sink panicked".to_string())
    );

    let source = r#"
import task
fn main() {
  let h = task.spawn({ -> println("x") })
  task.join(h)
  task.join(task.spawn({ -> 2 }))
}
"#;
    let ran = run_on_thread(source, HostIo::new(Panics, Buffer::new()));
    wait_until("the program ends", || ran.is_finished());
    assert_eq!(
        ran.join().unwrap(),
        Err(
            "joined task failed: cannot write to stdout: the output panicked: the sink panicked"
                .to_string()
        )
    );
}

/// A panic of the stderr output is dropped, like an error it returns:
/// the report is lost, the program's result stands.
#[test]
fn a_panicking_stderr_loses_the_report_only() {
    let out = Buffer::new();
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
    assert_eq!(
        run(source, HostIo::new(out.clone(), Panics)),
        Ok(Value::Unit)
    );
    assert_eq!(out.contents(), "main done\n");
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

// ── A clock that panics ─────────────────────────────────────────────

/// A clock that panics on every call once `broken` is set, and counts
/// its monotonic readings.
#[derive(Clone, Default)]
struct BreakingClock {
    broken: Arc<AtomicBool>,
    readings: Arc<AtomicU64>,
}

impl BreakingClock {
    fn check(&self) {
        if self.broken.load(Ordering::SeqCst) {
            panic!("the clock broke");
        }
    }
}

impl Clock for BreakingClock {
    fn now(&self) -> Duration {
        self.check();
        Duration::from_millis(NOON_MS)
    }

    fn monotonic(&self) -> Duration {
        self.check();
        self.readings.fetch_add(1, Ordering::SeqCst);
        Duration::ZERO
    }

    fn sleep(&self, _duration: Duration) {
        self.check();
    }
}

/// [`run`] on a thread of its own, with ten seconds to end: a program
/// that hangs fails the test instead of hanging it.
fn run_bounded(source: &'static str, io: HostIo) -> Result<Value, String> {
    let ran = run_on_thread(source, io);
    wait_until("the program ends", || ran.is_finished());
    ran.join().unwrap()
}

/// A clock that panics when a builtin reads it is a runtime error that
/// says so, whichever of its methods the builtin called.
#[test]
fn a_clock_that_panics_in_a_builtin_is_a_runtime_error() {
    let broken = || {
        let clock = BreakingClock::default();
        clock.broken.store(true, Ordering::SeqCst);
        HostIo::buffer(&Buffer::new()).clock(clock)
    };
    let failure = Err("the clock panicked: the clock broke".to_string());
    let now = "import time\nfn main() { time.now() }";
    assert_eq!(run_bounded(now, broken()), failure);
    let sleep = "import time\nfn main() { time.sleep(time.ms(5)) }";
    assert_eq!(run_bounded(sleep, broken()), failure);
    let timeout = "import channel\nfn main() { channel.receive(channel.timeout(5)) }";
    assert_eq!(run_bounded(timeout, broken()), failure);
}

/// A clock that starts to panic while the timer thread is reading it:
/// the task asleep on it is woken and fails with the clock's failure,
/// and the join of it returns.
#[test]
fn a_clock_that_panics_on_the_timer_thread_ends_the_waits() {
    let clock = BreakingClock::default();
    let source = r#"
import task
import time
fn main() {
  let h = task.spawn({ -> time.sleep(time.hours(1)) })
  task.join(h)
}
"#;
    let ran = run_on_thread(source, HostIo::buffer(&Buffer::new()).clock(clock.clone()));
    // The task reads the clock once for its deadline; every reading
    // after that is the timer thread's.
    wait_until("the timer thread reads the clock", || {
        clock.readings.load(Ordering::SeqCst) >= 5
    });
    clock.broken.store(true, Ordering::SeqCst);
    wait_until("the program ends", || ran.is_finished());
    assert_eq!(
        ran.join().unwrap(),
        Err("joined task failed: the clock panicked: the clock broke".to_string())
    );
}

/// The same on the main thread: a `channel.recv_timeout` that the
/// timer thread would have ended.
#[test]
fn a_clock_that_panics_on_the_timer_thread_ends_a_wait_of_main() {
    let clock = BreakingClock::default();
    let source = r#"
import channel
import time
fn main() {
  let quiet = channel.new(1)
  channel.send(quiet, 0)
  let _ = channel.receive(quiet)
  channel.recv_timeout(quiet, time.hours(1))
}
"#;
    let ran = run_on_thread(source, HostIo::buffer(&Buffer::new()).clock(clock.clone()));
    wait_until("the timer thread reads the clock", || {
        clock.readings.load(Ordering::SeqCst) >= 5
    });
    clock.broken.store(true, Ordering::SeqCst);
    wait_until("the program ends", || ran.is_finished());
    assert_eq!(
        ran.join().unwrap(),
        Err("the clock panicked: the clock broke".to_string())
    );
}

// ── One VM does not reach into another ──────────────────────────────

/// A clock whose time of day is `secs` after the epoch, for ever.
struct Fixed(u64);

impl Clock for Fixed {
    fn now(&self) -> Duration {
        Duration::from_secs(self.0)
    }
    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }
    fn sleep(&self, _duration: Duration) {}
}

/// The Unix time, in seconds, in the timestamp of the version 7 UUID
/// on the first line of `text`.
fn uuid_v7_secs(text: &str) -> u64 {
    let hex = text.lines().next().unwrap().replace('-', "");
    u64::from_str_radix(&hex[..12], 16).unwrap() / 1000
}

/// The timestamp of a `uuid.v7` is the time of its own VM's clock,
/// whatever clock another VM of the process has shown.
#[test]
fn uuid_v7_timestamps_are_each_vms_own() {
    let source = "import uuid\nfn main() { println(uuid.v7()) }";
    let (future, past, system) = (Buffer::new(), Buffer::new(), Buffer::new());
    // 2096, then 2001, then today.
    run(source, HostIo::buffer(&future).clock(Fixed(4_000_000_000))).unwrap();
    run(source, HostIo::buffer(&past).clock(Fixed(1_000_000_000))).unwrap();
    run(source, HostIo::buffer(&system)).unwrap();
    assert_eq!(uuid_v7_secs(&future.contents()), 4_000_000_000);
    assert_eq!(uuid_v7_secs(&past.contents()), 1_000_000_000);
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let stamped = uuid_v7_secs(&system.contents());
    assert!(
        today.abs_diff(stamped) < 60,
        "{stamped} is not about {today}"
    );
}

/// `math.random` is seeded from its own VM's clock: two VMs whose
/// clocks read the same give the same numbers, whatever ran before,
/// and a clock that reads zero still gives well-spread ones.
#[test]
fn math_random_is_seeded_per_vm_from_its_clock() {
    let source = r#"
import math
fn main() {
  println(math.random())
  println(math.random())
  println(math.random())
}
"#;
    let numbers = |clock: Fixed| -> Vec<f64> {
        let out = Buffer::new();
        run(source, HostIo::buffer(&out).clock(clock)).unwrap();
        let text = out.contents();
        text.lines().map(|line| line.parse().unwrap()).collect()
    };
    let first = numbers(Fixed(1_000));
    let other = numbers(Fixed(2_000));
    let again = numbers(Fixed(1_000));
    assert_eq!(first, again);
    assert_ne!(first, other);

    let zero = numbers(Fixed(0));
    assert_eq!(zero.len(), 3);
    assert!(zero.iter().all(|n| (0.0..1.0).contains(n)), "{zero:?}");
    assert!(zero[0] > 0.001, "{zero:?}");
    assert!(zero[0] != zero[1] && zero[1] != zero[2], "{zero:?}");
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
