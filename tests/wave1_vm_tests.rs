//! Regression locks for two VM defects.
//!
//! V1. A builtin passed directly as a callback (`list.map(chans,
//! channel.receive)`), or stored in a record field and called as a
//! method, left one stray value on the VM stack every time it parked
//! inside a task. The call that was being assembled around it then read
//! its function and its arguments from the wrong slots: the wrong function
//! was called, or a non-function was "called".
//!
//! V2. A method call and a function passed to a builtin run a nested
//! interpreter loop on the host stack. Recursion through either was
//! bounded only by the host stack, and running out of it aborted the
//! process. It now ends in the stack-overflow runtime error.
//!
//! Every test runs the built `silt` binary on a program in a fresh
//! temporary directory and asserts on its exit status and output. Each
//! run has a timeout, so a hang fails the test instead of hanging the
//! suite. The V1 programs depend on a task parking before its
//! counterpart delivers, so each runs several times; the assertions are
//! on results only, never on elapsed time.
//!
//! Tests named `guard_*` pin behaviour that was correct before and must
//! stay: they pass with and without the fix. All others fail without it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a scenario that depends on a task parking is run.
const REPEATS: usize = 5;

/// What the runtime error for exhausted recursion starts with, whichever
/// limit was hit.
const STACK_OVERFLOW: &str = "stack overflow: recursion depth exceeded";

/// What the Rust runtime prints when the host stack itself overflows and
/// the process is aborted.
const HOST_STACK_ABORT: &str = "overflowed its stack";

/// Recursion through a trait method: `n.go()` nests `n` method calls.
const DEEP_METHOD: &str = r#"
trait Deep { fn go(self) -> Int }
trait Deep for Int {
  fn go(self) -> Int {
    match self {
      0 -> 0
      _ -> 1 + (self - 1).go()
    }
  }
}
"#;

/// Recursion through a callback: `deep(n)` nests `n` `list.fold`
/// callbacks.
const DEEP_FOLD: &str = r#"
fn deep(n) {
  match n {
    0 -> 0
    _ -> list.fold([1], 0) { acc, x -> acc + x + deep(n - 1) }
  }
}
"#;

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
    let name = format!("silt_wave1_vm_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `src` to `<fresh dir>/work/<file_name>` and run
/// `silt <subcommand> <that file>` once.
///
/// Output goes to files outside the work directory rather than to pipes,
/// so a child that is killed on timeout cannot leave the test blocked on
/// a read.
fn run_silt(label: &str, subcommand: &str, file_name: &str, src: &str) -> Outcome {
    let dir = fresh_dir(label);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("create work dir");
    let source_path = work.join(file_name);
    std::fs::write(&source_path, src).expect("write source file");
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg(subcommand)
        .arg(&source_path)
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

/// `silt run` on a single program.
fn run_program(label: &str, src: &str) -> Outcome {
    run_silt(label, "run", "main.silt", src)
}

/// In each of `repeats` runs the program must finish in time, exit with
/// status 0, print exactly `expected` and print nothing to stderr.
fn assert_prints(label: &str, src: &str, expected: &str, repeats: usize) {
    for run in 1..=repeats {
        let outcome = run_program(label, src);
        assert!(
            !outcome.timed_out,
            "{label}, run {run}: did not finish within {RUN_TIMEOUT:?}: {outcome:?}"
        );
        assert_eq!(
            outcome.code,
            Some(0),
            "{label}, run {run}: expected exit status 0: {outcome:?}"
        );
        assert_eq!(
            outcome.stdout, expected,
            "{label}, run {run}: unexpected output: {outcome:?}"
        );
        assert_eq!(
            outcome.stderr, "",
            "{label}, run {run}: expected nothing on stderr: {outcome:?}"
        );
    }
}

/// The program must end with the stack-overflow runtime error: it
/// finishes in time, exits with status 1, prints nothing to stdout,
/// reports the error on stderr, and the host stack did not overflow.
/// Returns the outcome for further assertions.
fn assert_reports_stack_overflow(label: &str, src: &str) -> Outcome {
    let outcome = run_program(label, src);
    assert!(
        !outcome.timed_out,
        "{label}: did not finish within {RUN_TIMEOUT:?}: {outcome:?}"
    );
    assert!(
        !outcome.stderr.contains(HOST_STACK_ABORT),
        "{label}: the host stack overflowed and the process was aborted: {outcome:?}"
    );
    assert_eq!(
        outcome.code,
        Some(1),
        "{label}: expected the exit status of a runtime error: {outcome:?}"
    );
    assert!(
        outcome.stderr.contains("error[runtime]") && outcome.stderr.contains(STACK_OVERFLOW),
        "{label}: expected the stack-overflow runtime error on stderr: {outcome:?}"
    );
    assert_eq!(
        outcome.stdout, "",
        "{label}: expected nothing on stdout: {outcome:?}"
    );
    outcome
}

/// The line of `text` that ends with `suffix`, if there is one.
fn line_ending_with<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    text.lines().find(|line| line.trim_end().ends_with(suffix))
}

// ── V1: a builtin called as a function value parks ──────────────────

/// The program from the audit. The pending call `outer(inner, <list>)`
/// has `outer` and `inner` on the stack while `list.map` runs. With one
/// stray value left behind by the parked `channel.receive`, the call took
/// `inner` for its function.
#[test]
fn v1_builtin_callback_that_parks_does_not_shift_the_pending_call() {
    let src = r#"
import channel
import list
import task
import time
fn outer(g, ys) { "outer called with a function and {list.length(ys)} results" }
fn inner(c, ys) { "INNER called instead: second arg has {list.length(ys)} results" }
fn main() {
  let a = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
  }
  let worker = task.spawn { -> outer(inner, list.map([a], channel.receive)) }
  println(task.join(worker))
  task.join(sender)
}
"#;
    assert_prints(
        "v1_pending_call",
        src,
        "outer called with a function and 1 results\n",
        REPEATS,
    );
}

/// Two parks in one `list.map`, one per element. Used to fail with
/// "cannot call value of type Channel".
#[test]
fn v1_list_map_with_channel_receive_as_an_argument() {
    let src = r#"
import channel
import list
import task
import time
fn pair(x, ys) { (x, ys) }
fn main() {
  let a = channel.new(1)
  let b = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
    time.sleep(time.ms(20))
    channel.send(b, 2)
  }
  let worker = task.spawn { -> pair("x", list.map([a, b], channel.receive)) }
  println(task.join(worker))
  task.join(sender)
}
"#;
    assert_prints(
        "v1_receive_argument",
        src,
        "(x, [Message(1), Message(2)])\n",
        REPEATS,
    );
}

/// `task.join` as the callback, piped. Used to fail with "list.concat
/// requires a list or range".
#[test]
fn v1_list_map_with_task_join_piped_into_list_concat() {
    let src = r#"
import list
import task
import time
fn main() {
  let worker = task.spawn { ->
    let hs = [1, 2, 3] |> list.map { n -> task.spawn { ->
      time.sleep(time.ms(30))
      n * 10
    } }
    [0] |> list.concat(list.map(hs, task.join))
  }
  println(task.join(worker))
}
"#;
    assert_prints("v1_join_concat", src, "[0, 10, 20, 30]\n", REPEATS);
}

/// A park on a timer instead of on a channel: `time.sleep` as the
/// callback. Used to fail with "cannot call value of type Duration".
#[test]
fn v1_list_each_with_time_sleep_as_the_callback() {
    let src = r#"
import list
import task
import time
fn outer(g, y) { "outer called" }
fn inner(c, y) { "INNER called instead" }
fn main() {
  let worker = task.spawn { ->
    outer(inner, list.each([time.ms(10), time.ms(20)], time.sleep))
  }
  println(task.join(worker))
}
"#;
    assert_prints("v1_sleep_callback", src, "outer called\n", REPEATS);
}

/// The single-callback builtins (`option.map`, `result.map_ok`) call the
/// callback through a different helper than the iterating ones.
#[test]
fn v1_option_map_and_result_map_ok_with_a_parking_builtin() {
    let src = r#"
import channel
import option
import result
import task
import time
fn pair(x, y) { (x, y) }
fn main() {
  let a = channel.new(1)
  let b = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
    time.sleep(time.ms(20))
    channel.send(b, 2)
  }
  let first = task.spawn { -> pair("option", option.map(Some(a), channel.receive)) }
  println(task.join(first))
  let second = task.spawn { -> pair("result", result.map_ok(Ok(b), channel.receive)) }
  println(task.join(second))
  task.join(sender)
}
"#;
    assert_prints(
        "v1_single_callback",
        src,
        "(option, Some(Message(1)))\n(result, Ok(Message(2)))\n",
        REPEATS,
    );
}

/// `channel.each` keeps its own resume code. Here its callback is the
/// builtin `task.join`, which parks until the joined task is done.
#[test]
fn v1_channel_each_with_task_join_as_the_callback() {
    let src = r#"
import channel
import task
import time
fn outer(g, y) { "outer called" }
fn inner(c, y) { "INNER called instead" }
fn main() {
  let worker = task.spawn { ->
    let hs = channel.new(4)
    channel.send(hs, task.spawn { ->
      time.sleep(time.ms(30))
      1
    })
    channel.send(hs, task.spawn { ->
      time.sleep(time.ms(30))
      2
    })
    channel.close(hs)
    outer(inner, channel.each(hs, task.join))
  }
  println(task.join(worker))
}
"#;
    assert_prints("v1_channel_each", src, "outer called\n", REPEATS);
}

/// A builtin stored in a record field and called with method syntax
/// goes through the method-call instruction, not through a higher-order
/// builtin.
#[test]
fn v1_builtin_in_a_record_field_called_as_a_method() {
    let src = r#"
import channel
import task
import time
fn outer(g, y) { "outer called with {y}" }
fn inner(c, y) { "INNER called instead" }
fn main() {
  let a = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
  }
  let worker = task.spawn { ->
    let r = { recv: channel.receive }
    outer(inner, r.recv(a))
  }
  println(task.join(worker))
  task.join(sender)
}
"#;
    assert_prints(
        "v1_record_field",
        src,
        "outer called with Message(1)\n",
        REPEATS,
    );
}

/// The builtin passed as the callback is itself higher-order
/// (`list.map`), and the closure IT calls parks. On resume `list.fold`
/// has to continue `list.map`, and `list.map` its closure; each closure
/// body must run exactly once.
#[test]
fn v1_higher_order_builtin_as_the_callback_of_another() {
    let src = r#"
import channel
import list
import task
import time
fn pair(x, y) { (x, y) }
fn main() {
  let a = channel.new(1)
  let b = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
    time.sleep(time.ms(20))
    channel.send(b, 2)
  }
  let worker = task.spawn { ->
    let drain = { c ->
      let m = channel.receive(c)
      println("received {m}")
      c
    }
    let chans = list.fold([drain], [a, b], list.map)
    pair("x", list.length(chans))
  }
  println(task.join(worker))
  task.join(sender)
}
"#;
    assert_prints(
        "v1_nested_builtins",
        src,
        "received Message(1)\nreceived Message(2)\n(x, 2)\n",
        REPEATS,
    );
}

/// Guard: the same calls with the builtin wrapped in a closure always
/// worked.
#[test]
fn guard_v1_closure_around_the_parking_builtin() {
    let src = r#"
import channel
import list
import task
import time
fn outer(g, ys) { "outer called with a function and {list.length(ys)} results" }
fn inner(c, ys) { "INNER called instead: second arg has {list.length(ys)} results" }
fn pair(x, ys) { (x, ys) }
fn main() {
  let a = channel.new(1)
  let b = channel.new(1)
  let c = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
    time.sleep(time.ms(20))
    channel.send(b, 2)
    channel.send(c, 3)
  }
  let first = task.spawn { -> outer(inner, list.map([a]) { ch -> channel.receive(ch) }) }
  println(task.join(first))
  let second = task.spawn { -> pair("x", list.map([b, c]) { ch -> channel.receive(ch) }) }
  println(task.join(second))
  task.join(sender)
}
"#;
    assert_prints(
        "guard_v1_closure",
        src,
        "outer called with a function and 1 results\n(x, [Message(2), Message(3)])\n",
        REPEATS,
    );
}

/// Guard: on the main thread a builtin blocks instead of parking, so the
/// direct form always worked there.
#[test]
fn guard_v1_builtin_callback_on_the_main_thread() {
    let src = r#"
import channel
import list
import task
import time
fn outer(g, ys) { "outer called with a function and {list.length(ys)} results" }
fn inner(c, ys) { "INNER called instead: second arg has {list.length(ys)} results" }
fn main() {
  let a = channel.new(1)
  let sender = task.spawn { ->
    time.sleep(time.ms(50))
    channel.send(a, 1)
  }
  println(outer(inner, list.map([a], channel.receive)))
  task.join(sender)
}
"#;
    assert_prints(
        "guard_v1_main_thread",
        src,
        "outer called with a function and 1 results\n",
        REPEATS,
    );
}

// ── V2: recursion through method calls and callbacks ────────────────

#[test]
fn v2_method_recursion_on_the_main_thread_ends_in_a_runtime_error() {
    let src = format!(
        r#"{DEEP_METHOD}
fn main() {{
  println(50000.go())
}}
"#
    );
    assert_reports_stack_overflow("v2_method_main", &src);
}

#[test]
fn v2_callback_recursion_on_the_main_thread_ends_in_a_runtime_error() {
    let src = format!(
        r#"import list
{DEEP_FOLD}
fn main() {{
  println(deep(50000))
}}
"#
    );
    assert_reports_stack_overflow("v2_fold_main", &src);
}

#[test]
fn v2_method_recursion_in_a_task_ends_in_a_runtime_error() {
    let src = format!(
        r#"import task
{DEEP_METHOD}
fn main() {{
  let t = task.spawn {{ -> 50000.go() }}
  println(task.join(t))
}}
"#
    );
    assert_reports_stack_overflow("v2_method_task", &src);
}

#[test]
fn v2_callback_recursion_in_a_task_ends_in_a_runtime_error() {
    let src = format!(
        r#"import list
import task
{DEEP_FOLD}
fn main() {{
  let t = task.spawn {{ -> deep(50000) }}
  println(task.join(t))
}}
"#
    );
    assert_reports_stack_overflow("v2_fold_task", &src);
}

/// The error leaves the VM usable, and the levels it unwound are free
/// again: `silt test` runs both functions on one VM and one thread, the
/// first exhausts the budget, the second must still get its 500 levels.
#[test]
fn v2_a_test_run_continues_after_the_overflow() {
    let src = format!(
        r#"import test
{DEEP_METHOD}
fn test_a_recursion_far_beyond_the_budget() {{
  test.assert_eq(50000.go(), 50000)
}}
fn test_b_moderate_recursion_afterwards() {{
  test.assert_eq(500.go(), 500)
}}
"#
    );
    let label = "v2_silt_test";
    let outcome = run_silt(label, "test", "deep_test.silt", &src);
    assert!(
        !outcome.timed_out,
        "{label}: did not finish within {RUN_TIMEOUT:?}: {outcome:?}"
    );
    assert!(
        !outcome.stderr.contains(HOST_STACK_ABORT),
        "{label}: the host stack overflowed and the process was aborted: {outcome:?}"
    );
    assert_eq!(
        outcome.code,
        Some(1),
        "{label}: expected the exit status of a failed test run: {outcome:?}"
    );
    let first = line_ending_with(&outcome.stderr, "::test_a_recursion_far_beyond_the_budget");
    assert!(
        first.is_some_and(|line| line.trim_start().starts_with("FAIL")),
        "{label}: the first test must be reported as FAIL: {outcome:?}"
    );
    assert!(
        outcome.stderr.contains(STACK_OVERFLOW),
        "{label}: the first test must fail with the stack-overflow error: {outcome:?}"
    );
    let second = line_ending_with(&outcome.stderr, "::test_b_moderate_recursion_afterwards");
    assert!(
        second.is_some_and(|line| line.trim_start().starts_with("PASS")),
        "{label}: the second test must be reported as PASS: {outcome:?}"
    );
    assert!(
        outcome
            .stderr
            .contains("2 tests: 1 passed, 1 failed, 0 skipped"),
        "{label}: unexpected summary: {outcome:?}"
    );
}

/// Guard: recursion of moderate depth through a method and through a
/// callback works on the main thread.
#[test]
fn guard_v2_moderate_recursion_on_the_main_thread() {
    let src = format!(
        r#"import list
{DEEP_METHOD}
{DEEP_FOLD}
fn main() {{
  println("method {{500.go()}}")
  println("fold {{deep(500)}}")
}}
"#
    );
    assert_prints("guard_v2_main", &src, "method 500\nfold 500\n", 1);
}

/// Guard: recursion of moderate depth works inside a task. The depth is
/// one that fits the smallest stack a task can run on.
#[test]
fn guard_v2_moderate_recursion_in_a_task() {
    let src = format!(
        r#"import list
import task
{DEEP_METHOD}
{DEEP_FOLD}
fn main() {{
  let a = task.spawn {{ -> 6.go() }}
  let b = task.spawn {{ -> deep(6) }}
  println("method {{task.join(a)}}")
  println("fold {{task.join(b)}}")
}}
"#
    );
    assert_prints("guard_v2_task", &src, "method 6\nfold 6\n", 3);
}

/// Guard: a level is free again when its call has returned. 5000 calls
/// of `3.go()` one after the other never nest deeper than one of them.
#[test]
fn guard_v2_many_calls_in_sequence_are_not_depth() {
    let src = format!(
        r#"import list
import task
{DEEP_METHOD}
fn wide() {{
  list.fold(1..5000, 0) {{ acc, x -> acc + 3.go() }}
}}
fn main() {{
  println("main {{wide()}}")
  let t = task.spawn {{ -> wide() }}
  println("task {{task.join(t)}}")
}}
"#
    );
    assert_prints("guard_v2_sequence", &src, "main 15000\ntask 15000\n", 1);
}

/// Guard: a level is free again when its call has yielded. Each of the
/// 40 rounds parks six callback levels deep; the task resumes on
/// whichever worker thread is free and nests the six levels again.
#[test]
fn guard_v2_parking_inside_nested_callbacks_frees_the_levels() {
    let src = r#"
import list
import task
import time
fn nest(level, round) {
  match level {
    0 -> {
      time.sleep(time.ms(1))
      round
    }
    _ -> list.fold([1], 0) { acc, x -> acc + nest(level - 1, round) }
  }
}
fn main() {
  let t = task.spawn { ->
    list.fold(1..40, 0) { total, round -> total + nest(5, round) }
  }
  println(task.join(t))
}
"#;
    assert_prints("guard_v2_yield", src, "820\n", 3);
}

/// Guard: recursion through plain function calls is limited by the
/// number of VM frames, as before, and says so.
#[test]
fn guard_v2_plain_recursion_reports_the_frame_limit() {
    let src = r#"
fn deep(n) {
  match n {
    0 -> 0
    _ -> 1 + deep(n - 1)
  }
}
fn main() {
  println(deep(200000))
}
"#;
    let label = "guard_v2_plain";
    let outcome = assert_reports_stack_overflow(label, src);
    assert!(
        outcome
            .stderr
            .contains("stack overflow: recursion depth exceeded 100000 frames"),
        "{label}: expected the frame-limit wording: {outcome:?}"
    );
}
