//! What a running program pays for a value does not depend on what the
//! value holds.
//!
//! Each test runs two programs with the built `silt` and compares
//! their times: no time is asserted, only that the one is at most so
//! many times the other. A measurement over the cap is made again
//! (the machine may be busy), up to three times; what a program does
//! is over the cap every time. A run that is over the cap is stopped
//! where it gets there, so a program that takes hours where it should
//! take a second fails its test in minutes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A directory of the two programs of a test, removed with it.
struct Programs(PathBuf);

impl Programs {
    fn new(test: &str) -> Programs {
        let dir = std::env::temp_dir().join(format!(
            "silt_runtime_scaling_{test}_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        Programs(dir)
    }

    /// The program `source` as the file `name`.
    fn file(&self, name: &str, source: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, source).expect("the program is written");
        path
    }
}

impl Drop for Programs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How long `silt run` of the program takes; it must print `expected`.
/// `None` if the run has not ended after `limit`: it is stopped there.
fn run_time(file: &Path, expected: &str, limit: Option<Duration>) -> Option<Duration> {
    let started = Instant::now();
    let mut silt = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("silt runs");
    // (What a program of these tests prints is a line: it fits the
    // pipe, so the program ends without being read.)
    let elapsed = loop {
        if silt.try_wait().expect("silt is waited for").is_some() {
            break started.elapsed();
        }
        if limit.is_some_and(|limit| started.elapsed() > limit) {
            let _ = silt.kill();
            let _ = silt.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let output = silt.wait_with_output().expect("silt's output");
    assert!(
        output.status.success(),
        "the program runs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    Some(elapsed)
}

/// The best run times of `first` and of `second`, three runs of each,
/// in turn. A run of `first` that takes more than `cap` times the best
/// of `second` is stopped there, so `first` has no time if each of its
/// runs was.
fn best_times(
    cap: f64,
    first: (&Path, &str),
    second: (&Path, &str),
) -> (Option<Duration>, Duration) {
    let mut best_first: Option<Duration> = None;
    let mut best_second = Duration::MAX;
    for _ in 0..3 {
        let time = run_time(second.0, second.1, None).expect("a run without a limit ends");
        best_second = best_second.min(time);
        let limit = best_second.mul_f64(cap);
        if let Some(time) = run_time(first.0, first.1, Some(limit)) {
            best_first = Some(best_first.map_or(time, |best| best.min(time)));
        }
    }
    (best_first, best_second)
}

/// Assert that `first` takes at most `cap` times as long as `second`,
/// in one of three measurements; `what` says what it means if not.
fn assert_within(cap: f64, first: (&Path, &str), second: (&Path, &str), what: &str) {
    let mut over = Vec::new();
    for _ in 0..3 {
        match best_times(cap, first, second) {
            (Some(a), b) => {
                let ratio = a.as_secs_f64() / b.as_secs_f64();
                if ratio <= cap {
                    return;
                }
                over.push(format!("{a:?} against {b:?} ({ratio:.2})"));
            }
            (None, b) => over.push(format!("stopped each time, against {b:?}")),
        }
    }
    panic!(
        "{what}: more than {cap} times as long in each of three measurements ({})",
        over.join(", ")
    );
}

/// A list of six million elements, and `finds` times `list.find` for
/// an element that is the first.
fn finds_program(finds: usize) -> String {
    format!(
        "import list

fn finds(xs: List(Int), left: Int, found: Int) -> Int {{
  match left {{
    0 -> found
    _ -> {{
      let hit = list.find(xs) {{ x -> x > 0 }}
      match hit {{
        Some(_) -> finds(xs, left - 1, found + 1)
        None -> finds(xs, left - 1, found)
      }}
    }}
  }}
}}

fn main() {{
  let xs = list.reverse(1..6000000)
  println(finds(xs, {finds}, 0))
}}
"
    )
}

/// A builtin that calls a function for the elements of a list reads
/// the list as it goes. Each used to copy the whole list before its
/// first call, so forty `list.find` that hit the first of six million
/// elements took forty times as long as making the list.
#[test]
fn a_find_that_hits_the_first_element_does_not_read_the_list() {
    let programs = Programs::new("find");
    let forty = programs.file("forty.silt", &finds_program(40));
    let none = programs.file("none.silt", &finds_program(0));
    assert_within(
        2.0,
        (&forty, "40"),
        (&none, "0"),
        "making a list of 6,000,000 elements and finding its first element 40 times, \
         against making the list",
    );
}

/// A chain of `links` variants, each holding the next, made and then
/// walked by a match of each link.
fn chain_program(links: usize) -> String {
    format!(
        "type Chain {{
  End,
  Link(Int, Chain),
}}

fn chain(n: Int, acc: Chain) -> Chain {{
  match n {{
    0 -> acc
    _ -> chain(n - 1, Link(n, acc))
  }}
}}

fn total(c: Chain, acc: Int) -> Int {{
  match c {{
    End -> acc
    Link(n, rest) -> total(rest, acc + n)
  }}
}}

fn main() {{
  println(total(chain({links}, End), 0))
}}
"
    )
}

/// Reading a variant's field, and passing the variant on, shares it.
/// Each copied the variant with all it holds, so a walk of a chain
/// took the square of its length: thirty-two times the links took a
/// thousand times as long.
#[test]
fn a_walk_of_a_chain_of_variants_takes_time_as_its_length() {
    let programs = Programs::new("chain");
    let long = programs.file("long.silt", &chain_program(64_000));
    let short = programs.file("short.silt", &chain_program(2_000));
    assert_within(
        40.0,
        (&long, "2048032000"),
        (&short, "2001000"),
        "making and walking a chain of 64,000 variants, against one of 2,000",
    );
}
