//! Nesting limit for method calls and builtin callbacks.
//!
//! A method call, and a function passed to a builtin such as `list.fold`,
//! runs a nested interpreter loop on the host stack. The VM counts these
//! loops and refuses to start one more than the thread's stack can hold,
//! reporting "stack overflow: recursion depth exceeded N nested method or
//! callback calls" instead of letting the process abort. The limit is the
//! thread's stack size divided by an assumed cost per level, which is a
//! measured cost times a safety margin.
//!
//! These tests lock two facts, on the main thread and inside a task:
//!
//!   * The number N in the error is what the program can actually do: a
//!     recursion exactly N levels deep returns normally, and one level
//!     more gets the error naming N. On the main thread the error used to
//!     name one level more than the program could reach, because the loop
//!     running the program itself was counted.
//!   * The margin holds. The recursion to exactly N goes through the most
//!     expensive shapes measured (a `list.fold` callback, a trait method
//!     call, and the two alternating), so if a compiler or platform change
//!     makes a level cost more than the assumed cost allows for, the run
//!     at N overflows the real stack and the process aborts, and the test
//!     fails. This holds in whatever profile the tests are built in: the
//!     assumed cost differs between optimised and unoptimised builds, and
//!     each is checked by running the tests in that profile.
//!
//! N is read from the error of a run that recurses without bound, so the
//! tests follow whatever the stack size and cost per level are.
//!
//! Every test runs the built `silt` binary on a program in a fresh
//! temporary directory and asserts on its exit status and output. Each
//! run has a timeout, so a hang fails the test instead of hanging the
//! suite.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Upper bound for one run of the binary. A run that exceeds it is
/// killed and reported as a hang.
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

/// The error reported when the nesting limit is reached starts with this,
/// followed by the limit and `NESTED_SUFFIX`.
const OVERFLOW_PREFIX: &str = "stack overflow: recursion depth exceeded ";
const NESTED_SUFFIX: &str = " nested method or callback calls";

/// Depth for the run that finds the limit: far beyond any limit, but
/// small enough that the plain calls in between stay below the VM's
/// frame limit before the nesting limit is hit.
const UNBOUNDED: usize = 1_000_000;

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
    let name = format!("silt_wave2_vm_{pid}_{unique}_{label}");
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn read_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.replace("\r\n", "\n")
}

/// Write `src` to a fresh directory and `silt run` it once.
///
/// Output goes to files rather than to pipes, so a child that is killed
/// on timeout cannot leave the test blocked on a read.
fn run_program(label: &str, src: &str) -> Outcome {
    let dir = fresh_dir(label);
    let main = dir.join("main.silt");
    std::fs::write(&main, src).expect("write source file");
    let out_path = dir.join("stdout.txt");
    let err_path = dir.join("stderr.txt");
    let out_file = std::fs::File::create(&out_path).expect("create stdout file");
    let err_file = std::fs::File::create(&err_path).expect("create stderr file");

    let mut child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&main)
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

/// A recursion in which every level is one nested interpreter loop.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Every level is a `list.fold` callback, the most expensive level
    /// measured.
    Fold,
    /// Every level is a call of a trait method on a value of a bounded
    /// type variable, which the VM finds by the value's type and runs
    /// in a nested loop. (A method of a known impl is called like a
    /// function: it nests nothing.)
    Method,
    /// Levels alternate between such a method call and a `list.fold`
    /// callback.
    Mixed,
}

const SHAPES: [Shape; 3] = [Shape::Fold, Shape::Method, Shape::Mixed];

/// Where the recursion runs.
#[derive(Clone, Copy, Debug)]
enum Place {
    MainThread,
    Task,
}

/// Definitions of `deep(n)`, which nests exactly `n` interpreter loops
/// (for `n >= 1`) and returns `n`.
fn definitions(shape: Shape) -> &'static str {
    match shape {
        Shape::Fold => {
            "fn deep(n) {
  match n {
    0 -> 0
    _ -> list.fold([1], 0) { acc, x -> acc + x + deep(n - 1) }
  }
}
"
        }
        Shape::Method => {
            "trait Deep { fn go(self) -> Int }
trait Deep for Int {
  fn go(self) -> Int {
    match self {
      1 -> 1
      _ -> 1 + next(self - 1)
    }
  }
}
fn next(x: a) -> Int where a: Deep { x.go() }
fn deep(n) { next(n) }
"
        }
        Shape::Mixed => {
            "trait Deep { fn go(self) -> Int }
trait Deep for Int {
  fn go(self) -> Int { 1 + via_fold(self - 1) }
}
fn via_fold(n) {
  match n {
    0 -> 0
    _ -> list.fold([1], 0) { acc, x -> acc + x + via_method(n - 1) }
  }
}
fn via_method(n) {
  match n {
    0 -> 0
    _ -> next(n)
  }
}
fn next(x: a) -> Int where a: Deep { x.go() }
fn deep(n) { via_method(n) }
"
        }
    }
}

/// A program that prints `deep(depth)` for `shape`, run in `place`.
fn program(shape: Shape, place: Place, depth: usize) -> String {
    let mut src = String::new();
    if !matches!(shape, Shape::Method) {
        src.push_str("import list\n");
    }
    if matches!(place, Place::Task) {
        src.push_str("import task\n");
    }
    src.push_str(definitions(shape));
    match place {
        Place::MainThread => {
            src.push_str(&format!("fn main() {{\n  println(deep({depth}))\n}}\n"));
        }
        Place::Task => {
            src.push_str(&format!(
                "fn main() {{\n  let t = task.spawn {{ -> deep({depth}) }}\n  println(task.join(t))\n}}\n"
            ));
        }
    }
    src
}

/// Fail with a clear message if the process died instead of reporting.
fn assert_no_abort(ctx: &str, out: &Outcome) {
    assert!(
        !out.timed_out,
        "{ctx}: the program hung and was killed after {RUN_TIMEOUT:?}\n{out:#?}"
    );
    assert!(
        out.code.is_some() && !out.stderr.contains("overflowed its stack"),
        "{ctx}: the process aborted, most likely because the host stack overflowed \
         below the nesting limit. The cost per level the VM assumes \
         (NATIVE_STACK_BYTES_PER_LEVEL in src/vm/mod.rs) no longer covers a level \
         in this build: measure it again and raise it.\n{out:#?}"
    );
}

/// The limit named by the error of `out`, if it is the nesting error.
fn named_limit(out: &Outcome) -> Option<usize> {
    let start = out.stderr.find(OVERFLOW_PREFIX)? + OVERFLOW_PREFIX.len();
    let rest = &out.stderr[start..];
    let digits = rest.find(NESTED_SUFFIX)?;
    rest[..digits].parse().ok()
}

/// Recurse without bound through `list.fold` callbacks in `place` and
/// return the limit the error names.
fn limit_in(place: Place) -> usize {
    let ctx = format!("finding the limit in {place:?}");
    let out = run_program("probe", &program(Shape::Fold, place, UNBOUNDED));
    assert_no_abort(&ctx, &out);
    assert_eq!(
        out.code,
        Some(1),
        "{ctx}: unbounded recursion must end with the nesting error\n{out:#?}"
    );
    let limit = named_limit(&out)
        .unwrap_or_else(|| panic!("{ctx}: the error does not name a nesting limit\n{out:#?}"));
    assert!(
        limit >= 1,
        "{ctx}: the limit must allow one level\n{out:#?}"
    );
    limit
}

/// At exactly the named limit every shape returns normally; one level
/// more gets the nesting error naming the same limit.
fn assert_limit_is_exact_and_safe(place: Place) {
    let limit = limit_in(place);
    for shape in SHAPES {
        let ctx = format!("{shape:?} in {place:?}, {limit} levels (the named limit)");
        let out = run_program("at_limit", &program(shape, place, limit));
        assert_no_abort(&ctx, &out);
        assert_eq!(
            (out.code, out.stdout.as_str()),
            (Some(0), format!("{limit}\n").as_str()),
            "{ctx}: a recursion as deep as the limit the error names must return \
             normally\n{out:#?}"
        );

        let ctx = format!(
            "{shape:?} in {place:?}, {} levels (one past the limit)",
            limit + 1
        );
        let out = run_program("past_limit", &program(shape, place, limit + 1));
        assert_no_abort(&ctx, &out);
        assert_eq!(
            out.code,
            Some(1),
            "{ctx}: one level past the limit must end with the nesting error\n{out:#?}"
        );
        assert_eq!(
            named_limit(&out),
            Some(limit),
            "{ctx}: the error must name the same limit\n{out:#?}"
        );
        assert!(
            out.stdout.is_empty(),
            "{ctx}: nothing may be printed before the error\n{out:#?}"
        );
    }
}

#[test]
fn main_thread_nests_exactly_the_named_limit_without_aborting() {
    assert_limit_is_exact_and_safe(Place::MainThread);
}

#[test]
fn task_nests_exactly_the_named_limit_without_aborting() {
    assert_limit_is_exact_and_safe(Place::Task);
}
