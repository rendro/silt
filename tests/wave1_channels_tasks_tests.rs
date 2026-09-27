//! Channels and tasks: lost wake-ups, `task.cancel`, worker stacks,
//! timers in the deadlock detection, failures that nobody joins.
//!
//! Five defects are locked here.
//!
//!   * H1. A buffered channel woke a parked sender only when a receive
//!     found the buffer full. Several receives in a row woke one
//!     sender, and the other senders stayed parked for good: the
//!     worker-pool shape hung. The same class: a `channel.select` that
//!     was woken for one arm and completed another one used up the
//!     wake-up, and a send that raced a close could be accepted after
//!     the receiver had seen `Closed`.
//!   * H2. `task.cancel` could hang the runtime: the task handle ran
//!     the cancel cleanup while it held the lock on the cleanup, and
//!     the cleanup took the lock of the parked task, which the workers
//!     and the wakers take first.
//!   * H3. Scheduler workers had the default stack of 2 MiB, so
//!     recursion through a callback inside `task.spawn` overflowed the
//!     native stack at a small depth and aborted the process.
//!   * H4. A task that waits for a timer was counted as stuck, so a
//!     main thread that waited for that task's send was told
//!     "deadlock".
//!   * H5. A task that failed and was never joined left no trace.
//!
//! Every test runs the built `silt` binary on a program in a fresh
//! temporary directory and asserts on exit status, stdout and stderr.
//! Every run has a kill timeout, so a hang fails the test instead of
//! hanging the suite. The defects are races, so every scenario runs
//! several times: ten times where the defect is a hang, three times
//! otherwise. The assertions are on results, never on elapsed time.
//!
//! Tests named `guard_*` pin behaviour that must not change; they pass
//! with and without the fixes. All other tests fail without them. Where
//! the defect is a race, "fail" means that a run fails with the
//! frequency given at the test, and the test runs the scenario often
//! enough to meet a failing run; the rarest one is
//! `h1_value_accepted_before_close_is_delivered`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and counts as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a scenario runs whose defect is a hang.
const HANG_REPEATS: usize = 10;

/// How often every other scenario runs.
const REPEATS: usize = 3;

/// Every main-thread deadlock diagnostic starts with this.
const DEADLOCK: &str = "deadlock on main thread";

/// Part of every report of a task that failed and was not joined.
const NEVER_JOINED: &str = "failed and was never joined";

#[derive(Debug)]
struct Outcome {
    /// Exit status; `None` if the process was ended by a signal.
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// True if the run exceeded `RUN_TIMEOUT` and was killed.
    timed_out: bool,
}

fn fresh_dir(label: &str) -> PathBuf {
    static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let unique = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
    let name = format!("silt_wave1_channels_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `src` into a fresh directory and run `silt run` on it once.
///
/// Output goes to files outside the program's directory rather than to
/// pipes, so a child that is killed on timeout cannot leave the test
/// blocked on a read.
fn run_program(label: &str, src: &str) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    let program = work.join("main.silt");
    std::fs::write(&program, src).expect("write program");
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&program)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
        .expect("spawn silt");

    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status,
            None if started.elapsed() >= RUN_TIMEOUT => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().expect("wait after kill");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let outcome = Outcome {
        code: status.code(),
        stdout: read_text(&out_path),
        stderr: read_text(&err_path),
        timed_out,
    };
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// In each of `repeats` runs the program must finish in time, report
/// no deadlock, exit with status 0 and print exactly `expected`.
/// Returns the outcomes, for further assertions.
fn assert_completes(label: &str, src: &str, expected: &str, repeats: usize) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(repeats);
    for run in 1..=repeats {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {repeats}");
        assert!(
            !out.timed_out,
            "{ctx}: the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: the program was told `{DEADLOCK}`, but it can complete\n{out:#?}"
        );
        assert_eq!(
            out.code,
            Some(0),
            "{ctx}: the program must exit with status 0\n{out:#?}"
        );
        assert_eq!(
            out.stdout, expected,
            "{ctx}: the program printed something else than expected\n{out:#?}"
        );
        outcomes.push(out);
    }
    outcomes
}

/// In each of `repeats` runs the program must end with the main-thread
/// deadlock diagnostic: in time (a hang is a failure), with a non-zero
/// exit status, after printing exactly `expected`. Returns the
/// outcomes, for further assertions.
fn assert_reports_deadlock(label: &str, src: &str, expected: &str, repeats: usize) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(repeats);
    for run in 1..=repeats {
        let out = run_program(label, src);
        let ctx = format!("{label}, run {run} of {repeats}");
        assert!(
            !out.timed_out,
            "{ctx}: the program hung instead of reporting the deadlock; \
             killed after {RUN_TIMEOUT:?}\n{out:#?}"
        );
        assert!(
            out.stderr.contains(DEADLOCK),
            "{ctx}: expected the `{DEADLOCK}` diagnostic on stderr\n{out:#?}"
        );
        assert_ne!(
            out.code,
            Some(0),
            "{ctx}: a deadlocked program must exit with a non-zero status\n{out:#?}"
        );
        assert_eq!(
            out.stdout, expected,
            "{ctx}: the program printed something else than expected\n{out:#?}"
        );
        outcomes.push(out);
    }
    outcomes
}

// ── H1. Buffered channels: every receive wakes a sender ──────────────

/// The worker-pool shape: 40 tasks send one result each into a channel
/// with room for 10, the main thread collects 40. Without the fix the
/// senders that are parked behind the full buffer are woken one per
/// "buffer was full", not one per receive, and most runs hang.
const WORKER_POOL: &str = r#"
import channel
import task
import list
fn collect(ch, n, acc) {
  match n {
    0 -> acc
    _ -> {
      match channel.receive(ch) {
        Message(v) -> collect(ch, n - 1, acc + v)
        _ -> acc
      }
    }
  }
}
fn main() {
  let results = channel.new(CAPACITY)
  let ids = 1..40
  ids |> list.each { i ->
    let _ = task.spawn(fn() { channel.send(results, i * i) })
    ()
  }
  let total = collect(results, 40, 0)
  println("total {total}")
}
"#;

#[test]
fn h1_worker_pool_with_more_senders_than_capacity_completes() {
    let src = WORKER_POOL.replace("CAPACITY", "10");
    assert_completes("h1_worker_pool_cap10", &src, "total 22140\n", HANG_REPEATS);
}

/// Control for the test above: with room for all 40 results no sender
/// ever parks. Passes with and without the fix.
#[test]
fn guard_worker_pool_with_capacity_for_all_completes() {
    let src = WORKER_POOL.replace("CAPACITY", "40");
    assert_completes(
        "guard_worker_pool_cap40",
        &src,
        "total 22140\n",
        HANG_REPEATS,
    );
}

/// Two values are buffered and four senders are parked when the main
/// thread starts to receive. Without the fix the run stops after three
/// or four values, every time.
#[test]
fn h1_main_receives_from_senders_parked_behind_a_full_buffer() {
    let src = r#"
import channel
import task
import time
import list

fn main() {
  let ch = channel.new(2)
  let senders = [1, 2, 3, 4, 5, 6] |> list.map { i ->
    task.spawn(fn() { channel.send(ch, i) })
  }
  time.sleep(time.ms(200))
  loop n = 0 {
    match n < 6 {
      true -> {
        match channel.receive(ch) {
          Message(_) -> println("got one")
          _ -> println("got none")
        }
        loop(n + 1)
      }
      false -> ()
    }
  }
  senders |> list.each { s -> task.join(s) }
  println("done")
}
"#;
    assert_completes(
        "h1_parked_senders_main",
        src,
        "got one\ngot one\ngot one\ngot one\ngot one\ngot one\ndone\n",
        HANG_REPEATS,
    );
}

/// The same with a task as the receiver. Without the fix this hangs
/// without a deadlock report, because the parked senders count as
/// counterparties of the receiver.
#[test]
fn h1_task_receives_from_senders_parked_behind_a_full_buffer() {
    let src = r#"
import channel
import task
import time
import list

fn recv_n(ch, n, acc) {
  match n {
    0 -> acc
    _ -> {
      match channel.receive(ch) {
        Message(v) -> recv_n(ch, n - 1, acc + v)
        _ -> acc
      }
    }
  }
}

fn main() {
  let ch = channel.new(3)
  let senders = [1, 2, 3, 4, 5, 6, 7, 8] |> list.map { i ->
    task.spawn(fn() { channel.send(ch, i) })
  }
  time.sleep(time.ms(200))
  let receiver = task.spawn(fn() { recv_n(ch, 8, 0) })
  let total = task.join(receiver)
  println("total {total}")
}
"#;
    assert_completes("h1_parked_senders_task", src, "total 36\n", HANG_REPEATS);
}

/// A worker pool in the documented `loop _ = () { ... loop(()) }`
/// form: three workers take jobs and send results, both channels are
/// smaller than the number of jobs. Without the fix two of the three
/// workers stay parked on the results channel with their results, and
/// the main thread waits for results that never arrive.
#[test]
fn h1_worker_pool_in_loop_form_completes() {
    let src = r#"
import channel
import list
import task

fn collect(ch, n, acc) {
  match n {
    0 -> acc
    _ -> {
      match channel.receive(ch) {
        Message(v) -> collect(ch, n - 1, acc + v)
        _ -> acc
      }
    }
  }
}

fn main() {
  let jobs = channel.new(4)
  let results = channel.new(4)

  let workers = [1, 2, 3] |> list.map { id ->
    task.spawn(fn() {
      loop _ = () {
        match channel.receive(jobs) {
          Message(n) -> {
            channel.send(results, n * 2)
            loop(())
          }
          _ -> ()
        }
      }
    })
  }

  let producer = task.spawn(fn() {
    1..30 |> list.each { n -> channel.send(jobs, n) }
    channel.close(jobs)
  })

  let total = collect(results, 30, 0)
  task.join(producer)
  workers |> list.each { w -> task.join(w) }
  println("total {total}")
}
"#;
    assert_completes("h1_worker_loop", src, "total 930\n", HANG_REPEATS);
}

/// The documented multiplexing example, in the documented
/// `loop _ = () { ... loop(()) }` form, on the main thread. Which
/// messages it prints before a channel reports `Closed` depends on the
/// order in which the two tasks run, so only the end is pinned. Passes
/// with and without the fix.
#[test]
fn guard_documented_select_loop_ends_when_a_channel_closes() {
    let src = r#"
import channel
import task
fn main() {
  let alerts = channel.new(5)
  let logs = channel.new(5)

  let _ = task.spawn(fn() {
    channel.send(logs, "background task done")
    channel.send(logs, "log rotation complete")
    channel.close(logs)
  })

  let _ = task.spawn(fn() {
    channel.send(alerts, "disk full!")
    channel.close(alerts)
  })

  loop _ = () {
    match channel.select([Recv(alerts), Recv(logs)]) {
      (^alerts, Message(msg)) -> {
        println("alert: {msg}")
        loop(())
      }
      (^logs,   Message(msg)) -> {
        println("log: {msg}")
        loop(())
      }
      (_, Closed) -> {
        println("a channel closed")
        return ()
      }
      _ -> loop(())
    }
  }
}
"#;
    for run in 1..=HANG_REPEATS {
        let out = run_program("guard_select_loop", src);
        let ctx = format!("guard_select_loop, run {run} of {HANG_REPEATS}");
        assert!(!out.timed_out, "{ctx}: the program hung\n{out:#?}");
        assert_eq!(out.code, Some(0), "{ctx}: exit status\n{out:#?}");
        assert!(
            !out.stderr.contains(DEADLOCK),
            "{ctx}: false deadlock report\n{out:#?}"
        );
        assert!(
            out.stdout.ends_with("a channel closed\n"),
            "{ctx}: the loop must end with the closed channel\n{out:#?}"
        );
        for line in out.stdout.lines() {
            assert!(
                line == "a channel closed"
                    || line == "alert: disk full!"
                    || line == "log: background task done"
                    || line == "log: log rotation complete",
                "{ctx}: unexpected line {line:?}\n{out:#?}"
            );
        }
    }
}

/// r1 waits for `a` or `b`, r2 waits for `a` behind r1. One value goes
/// to each channel. The value in `a` wakes r1. If r1 then takes the
/// value of `b`, the value in `a` is r2's, but r2's wake-up was used up
/// by r1: without the fix r2 stays parked next to its value and the
/// join is told "deadlock" (about half of the runs). If r1 takes the
/// value of `a`, the program sends another one, so that r2 can finish
/// in both cases.
#[test]
fn h1_select_that_takes_another_arm_passes_its_wake_up_on() {
    let src = r#"
import channel
import task
import time

fn main() {
  let a = channel.new(4)
  let b = channel.new(4)
  let r1 = task.spawn(fn() {
    match channel.select([Recv(a), Recv(b)]) {
      (^a, Message(_)) -> "a"
      (^b, Message(_)) -> "b"
      _ -> "other"
    }
  })
  time.sleep(time.ms(50))
  let r2 = task.spawn(fn() {
    match channel.receive(a) {
      Message(v) -> v
      _ -> 0 - 1
    }
  })
  time.sleep(time.ms(50))
  channel.send(a, 1)
  channel.send(b, 2)
  let took = task.join(r1)
  match took {
    "a" -> channel.send(a, 1)
    _ -> ()
  }
  let got = task.join(r2)
  println("r2 got {got}")
}
"#;
    assert_completes("h1_select_baton", src, "r2 got 1\n", HANG_REPEATS);
}

/// A producer counts the sends that the channel accepted, a consumer
/// counts the values it received until `Closed`, and the main thread
/// closes the channel in the middle. Every accepted value must arrive.
/// Without the fix a send can be accepted after the consumer has seen
/// `Closed`; that is rare (about one run in ten loses a value).
#[test]
fn h1_value_accepted_before_close_is_delivered() {
    let src = r#"
import channel
import task

fn spin(n) {
  match n {
    0 -> 0
    _ -> spin(n - 1)
  }
}

fn consume(ch, n) {
  match channel.receive(ch) {
    Message(_) -> consume(ch, n + 1)
    _ -> n
  }
}

fn produce(ch, n) {
  match channel.select([Send(ch, n)]) {
    (_, Sent) -> produce(ch, n + 1)
    _ -> n
  }
}

fn go(i, rounds, lost) {
  match i < rounds {
    true -> {
      let ch = channel.new(8)
      let c = task.spawn(fn() { consume(ch, 0) })
      let s = task.spawn(fn() { produce(ch, 0) })
      let _ = spin(200 + i % 97)
      channel.close(ch)
      let sent = task.join(s)
      let received = task.join(c)
      match sent > received {
        true -> go(i + 1, rounds, lost + sent - received)
        false -> go(i + 1, rounds, lost)
      }
    }
    false -> lost
  }
}

fn main() {
  let lost = go(0, 100, 0)
  println("values accepted and never delivered: {lost}")
}
"#;
    assert_completes(
        "h1_close_race",
        src,
        "values accepted and never delivered: 0\n",
        HANG_REPEATS,
    );
}

// ── H2. task.cancel ──────────────────────────────────────────────────

/// Cancel a task while it is about to park, or has just parked, in
/// `channel.receive`. Without the fix about half of the runs hang
/// inside `task.cancel`.
#[test]
fn h2_cancel_of_a_task_that_is_parking_returns() {
    let src = r#"
import channel
import task

fn spin(n) {
  match n {
    0 -> 0
    _ -> spin(n - 1)
  }
}

fn go(i, n) {
  match i < n {
    true -> {
      let ch = channel.new(1)
      let h = task.spawn(fn() { channel.receive(ch) })
      let _ = spin(i % 40)
      task.cancel(h)
      go(i + 1, n)
    }
    false -> ()
  }
}

fn main() {
  go(0, 200)
  println("completed 200 rounds")
}
"#;
    assert_completes(
        "h2_cancel_setup",
        src,
        "completed 200 rounds\n",
        HANG_REPEATS,
    );
}

/// A task is parked in `channel.receive`; a second task wakes it while
/// the main thread cancels it. Without the fix about two of three runs
/// hang inside `task.cancel`.
#[test]
fn h2_cancel_that_races_a_wake_up_returns() {
    let src = r#"
import channel
import task
import time

fn spin(n) {
  match n {
    0 -> 0
    _ -> spin(n - 1)
  }
}

fn go(i, n) {
  match i < n {
    true -> {
      let ch = channel.new(1)
      let t = task.spawn(fn() { channel.receive(ch) })
      time.sleep(time.ms(2))
      let s = task.spawn(fn() { channel.try_send(ch, i) })
      let _ = spin(i % 30)
      task.cancel(t)
      let _ = task.join(s)
      go(i + 1, n)
    }
    false -> ()
  }
}

fn main() {
  go(0, 100)
  println("completed 100 rounds")
}
"#;
    assert_completes(
        "h2_cancel_vs_wake",
        src,
        "completed 100 rounds\n",
        HANG_REPEATS,
    );
}

/// The same race with a task that is parked in `channel.select`. The
/// select arm of the scheduler keeps the waker registrations of its
/// arms apart from the other park arms, so it has a test of its own.
/// Without the fix nine of ten runs hang inside `task.cancel`.
#[test]
fn h2_cancel_of_a_parked_select_that_races_a_wake_up_returns() {
    let src = r#"
import channel
import task
import time

fn spin(n) {
  match n {
    0 -> 0
    _ -> spin(n - 1)
  }
}

fn go(i, n) {
  match i < n {
    true -> {
      let a = channel.new(1)
      let b = channel.new(1)
      let t = task.spawn(fn() { channel.select([Recv(a), Recv(b)]) })
      time.sleep(time.ms(2))
      let s = task.spawn(fn() { channel.try_send(b, i) })
      let _ = spin(i % 30)
      task.cancel(t)
      let _ = task.join(s)
      go(i + 1, n)
    }
    false -> ()
  }
}

fn main() {
  go(0, 100)
  println("completed 100 rounds")
}
"#;
    assert_completes(
        "h2_cancel_select_vs_wake",
        src,
        "completed 100 rounds\n",
        HANG_REPEATS,
    );
}

// ── H3. Worker stacks ────────────────────────────────────────────────

/// Recursion through a callback, 100 levels deep. Every level runs
/// inside `list.map`, so every level nests native frames.
const NESTED_CALLBACKS: &str = r#"
import list
import task

fn nest(n) {
  match n {
    0 -> 0
    _ -> {
      let inner = [n - 1] |> list.map { m -> nest(m) }
      match inner {
        [v] -> v + 1
        _ -> 0 - 1
      }
    }
  }
}

fn main() {
  RUN
}
"#;

/// Inside `task.spawn` the recursion must reach the depth that it
/// reaches on the main thread. Without the fix the worker's 2 MiB
/// stack overflows and the process aborts.
#[test]
fn h3_recursion_through_callbacks_in_a_task_does_not_overflow_the_stack() {
    let src = NESTED_CALLBACKS.replace(
        "RUN",
        r#"let h = task.spawn(fn() { nest(100) })
  println("depth {task.join(h)}")"#,
    );
    let outcomes = assert_completes("h3_task_depth", &src, "depth 100\n", REPEATS);
    for out in outcomes {
        assert!(
            !out.stderr.contains("overflowed its stack"),
            "the native stack of the worker overflowed\n{out:#?}"
        );
    }
}

/// The same recursion on the main thread. Passes with and without the
/// fix.
#[test]
fn guard_recursion_through_callbacks_on_main_completes() {
    let src = NESTED_CALLBACKS.replace("RUN", r#"println("depth {nest(100)}")"#);
    assert_completes("guard_main_depth", &src, "depth 100\n", REPEATS);
}

// ── H4. A pending timer is a wake source ─────────────────────────────

/// A task waits for `channel.timeout(600)` and then sends to the main
/// thread, which waits for that send. Without the fix the main thread
/// is told "deadlock" while the timer is running.
#[test]
fn h4_main_waits_for_a_task_that_waits_for_a_timer() {
    let src = r#"
import channel
import task

fn main() {
  let x = channel.new(0)
  let h = task.spawn(fn() {
    let t = channel.timeout(600)
    let _ = channel.receive(t)
    channel.send(x, 1)
  })
  match channel.receive(x) {
    Message(v) -> println("main got: {v}")
    _ -> println("main got no message")
  }
  task.join(h)
}
"#;
    assert_completes("h4_timer", src, "main got: 1\n", REPEATS);
}

/// The same with `channel.recv_timeout`, which parks the task on a
/// select with a timer arm.
#[test]
fn h4_main_waits_for_a_task_that_waits_in_recv_timeout() {
    let src = r#"
import channel
import task
import time

fn main() {
  let quiet = channel.new(1)
  let x = channel.new(0)
  let h = task.spawn(fn() {
    let waited = match channel.recv_timeout(quiet, time.ms(600)) {
      Ok(_) -> "value"
      Err(ChannelTimeout) -> "timed out"
      Err(ChannelClosed) -> "closed"
    }
    channel.send(x, waited)
  })
  match channel.receive(x) {
    Message(v) -> println("main got: {v}")
    _ -> println("main got no message")
  }
  task.join(h)
}
"#;
    assert_completes("h4_recv_timeout", src, "main got: timed out\n", REPEATS);
}

/// The main thread joins a task that waits for the send of a task that
/// waits for a timer. Without the fix the join is told "deadlock".
#[test]
fn h4_main_joins_a_task_that_depends_on_a_timer_task() {
    let src = r#"
import channel
import task

fn main() {
  let x = channel.new(0)
  let waiter = task.spawn(fn() {
    let t = channel.timeout(600)
    let _ = channel.receive(t)
    channel.send(x, 7)
  })
  let relay = task.spawn(fn() {
    match channel.receive(x) {
      Message(v) -> v * 2
      _ -> 0 - 1
    }
  })
  let got = task.join(relay)
  task.join(waiter)
  println("main got: {got}")
}
"#;
    assert_completes("h4_join_timer", src, "main got: 14\n", REPEATS);
}

/// A real deadlock is still reported when a timer was involved: the
/// task waits for a timer, and afterwards for a channel that nobody
/// sends on. The report comes after the timer has fired. Passes with
/// and without the fix.
#[test]
fn guard_deadlock_after_a_timer_has_fired_is_reported() {
    let src = r#"
import channel
import task

fn main() {
  let x = channel.new(0)
  let dead = channel.new(0)
  let h = task.spawn(fn() {
    let t = channel.timeout(100)
    let _ = channel.receive(t)
    println("timer fired")
    let _ = channel.receive(dead)
    channel.send(x, 1)
  })
  match channel.receive(x) {
    Message(v) -> println("main got: {v}")
    _ -> println("main got no message")
  }
}
"#;
    assert_reports_deadlock("guard_deadlock_after_timer", src, "timer fired\n", REPEATS);
}

// ── H5. A failed task that nobody joins is reported ──────────────────

/// The failure of a task that is never joined is reported on stderr,
/// with its error. The exit status stays 0.
///
/// The main thread cannot wait for the failure without joining the
/// task, so it sleeps. The sleep is long, to leave the task time to
/// fail on a loaded machine; nothing is asserted about time.
#[test]
fn h5_failed_task_that_is_never_joined_is_reported() {
    let src = r#"
import task
import time
fn main() {
  let _ = task.spawn(fn() { panic("worker failed") })
  time.sleep(time.ms(500))
  println("main done")
}
"#;
    let outcomes = assert_completes("h5_unjoined", src, "main done\n", REPEATS);
    for out in outcomes {
        assert!(
            out.stderr.contains(NEVER_JOINED),
            "the failed task must be reported on stderr\n{out:#?}"
        );
        assert!(
            out.stderr.contains("worker failed"),
            "the report must contain the task's error\n{out:#?}"
        );
        assert_eq!(
            out.stderr.matches(NEVER_JOINED).count(),
            1,
            "one failed task, one report\n{out:#?}"
        );
    }
}

/// The main thread keeps the handle of the failed task in a variable
/// and never joins it, and another task is still parked when the
/// program ends.
///
/// The task tells the main thread that it is about to fail, and fails
/// in its next step; the main thread sleeps after that message.
#[test]
fn h5_failure_is_reported_when_the_handle_is_kept_and_a_task_is_still_parked() {
    let src = r#"
import channel
import task
import time

fn main() {
  let never = channel.new(0)
  let about_to_fail = channel.new(1)
  let idle = task.spawn(fn() { channel.receive(never) })
  let failing = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    let zero = 0
    10 / zero
  })
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  println("main done")
}
"#;
    let outcomes = assert_completes("h5_kept_handle", src, "main done\n", REPEATS);
    for out in outcomes {
        assert!(
            out.stderr.contains(NEVER_JOINED),
            "the failed task must be reported on stderr\n{out:#?}"
        );
        assert!(
            out.stderr.contains("division"),
            "the report must contain the task's error\n{out:#?}"
        );
    }
}

/// Every task that failed and was not joined gets its report.
#[test]
fn h5_every_unjoined_failure_is_reported() {
    let src = r#"
import channel
import task
import time
fn main() {
  let about_to_fail = channel.new(2)
  let _ = task.spawn(fn() {
    channel.send(about_to_fail, 1)
    panic("first worker failed")
  })
  let _ = task.spawn(fn() {
    channel.send(about_to_fail, 2)
    panic("second worker failed")
  })
  let fine = task.spawn(fn() { 42 })
  let _ = channel.receive(about_to_fail)
  let _ = channel.receive(about_to_fail)
  time.sleep(time.ms(300))
  println("main done {task.join(fine)}")
}
"#;
    let outcomes = assert_completes("h5_two_failures", src, "main done 42\n", REPEATS);
    for out in outcomes {
        assert!(
            out.stderr.contains("first worker failed"),
            "the first failure must be reported\n{out:#?}"
        );
        assert!(
            out.stderr.contains("second worker failed"),
            "the second failure must be reported\n{out:#?}"
        );
        assert_eq!(
            out.stderr.matches(NEVER_JOINED).count(),
            2,
            "two failed tasks, two reports\n{out:#?}"
        );
    }
}

/// A producer fails before it sends, the consumer waits for it, and
/// the main thread joins the consumer. The deadlock diagnostic is the
/// consequence; the report names the cause.
#[test]
fn h5_deadlock_that_follows_from_a_failed_task_shows_the_failure() {
    let src = r#"
import channel
import task

fn main() {
  let ch = channel.new(0)
  let consumer = task.spawn(fn() {
    match channel.receive(ch) {
      Message(v) -> "got {v}"
      Closed -> "closed"
      _ -> "other"
    }
  })
  let producer = task.spawn(fn() {
    let zero = 0
    channel.send(ch, 10 / zero)
  })
  println(task.join(consumer))
}
"#;
    let outcomes = assert_reports_deadlock("h5_deadlock_cause", src, "", REPEATS);
    for out in outcomes {
        assert!(
            out.stderr.contains(NEVER_JOINED),
            "the failed producer must be reported on stderr\n{out:#?}"
        );
        assert!(
            out.stderr.contains("division"),
            "the report must contain the producer's error\n{out:#?}"
        );
    }
}

/// A failure that a join received is the joiner's: it is raised there
/// and not reported a second time. Passes with and without the fix.
#[test]
fn guard_joined_failure_is_raised_by_the_join_only() {
    let src = r#"
import task
import time

fn main() {
  let failing = task.spawn(fn() { panic("worker failed") })
  time.sleep(time.ms(100))
  let v = task.join(failing)
  println("not reached {v}")
}
"#;
    for run in 1..=REPEATS {
        let out = run_program("guard_joined_failure", src);
        let ctx = format!("guard_joined_failure, run {run} of {REPEATS}");
        assert!(!out.timed_out, "{ctx}: the program hung\n{out:#?}");
        assert_ne!(out.code, Some(0), "{ctx}: the join must fail\n{out:#?}");
        assert_eq!(out.stdout, "", "{ctx}: stdout\n{out:#?}");
        assert!(
            out.stderr.contains("joined task failed"),
            "{ctx}: the join must raise the task's error\n{out:#?}"
        );
        assert!(
            out.stderr.contains("worker failed"),
            "{ctx}: the error must be the task's\n{out:#?}"
        );
        assert!(
            !out.stderr.contains(NEVER_JOINED),
            "{ctx}: a joined failure is not an unjoined one\n{out:#?}"
        );
    }
}

/// A cancelled task did not fail, and a task that ended well has
/// nothing to report either. Passes with and without the fix.
#[test]
fn guard_cancelled_and_finished_tasks_are_not_reported() {
    let src = r#"
import channel
import task
import time

fn main() {
  let never = channel.new(0)
  let parked = task.spawn(fn() { channel.receive(never) })
  let done = task.spawn(fn() { 42 })
  time.sleep(time.ms(100))
  task.cancel(parked)
  time.sleep(time.ms(100))
  println("main done")
}
"#;
    let outcomes = assert_completes("guard_cancelled", src, "main done\n", REPEATS);
    for out in outcomes {
        assert!(
            !out.stderr.contains(NEVER_JOINED),
            "neither task failed\n{out:#?}"
        );
    }
}
