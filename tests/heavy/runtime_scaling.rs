//! What a running program pays for a value does not depend on what the
//! value holds.
//!
//! Each test runs two programs with the built `silt` and compares
//! their times: no time is asserted, only that the one is at most so
//! many times the other. A measurement over the cap is made again
//! (the machine may be busy), up to three times; what a program does
//! is over the cap every time.

use std::path::{Path, PathBuf};
use std::process::Command;
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
fn run_time(file: &Path, expected: &str) -> Duration {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(file)
        .output()
        .expect("silt runs");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "the program runs: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    elapsed
}

/// The best run times of `first` and of `second`, three runs of each,
/// in turn.
fn best_times(first: (&Path, &str), second: (&Path, &str)) -> (Duration, Duration) {
    let mut best_first = Duration::MAX;
    let mut best_second = Duration::MAX;
    for _ in 0..3 {
        best_first = best_first.min(run_time(first.0, first.1));
        best_second = best_second.min(run_time(second.0, second.1));
    }
    (best_first, best_second)
}

/// Assert that `first` takes at most `cap` times as long as `second`,
/// in one of three measurements; `what` says what it means if not.
fn assert_within(cap: f64, first: (&Path, &str), second: (&Path, &str), what: &str) {
    let mut over = Vec::new();
    for _ in 0..3 {
        let (a, b) = best_times(first, second);
        let ratio = a.as_secs_f64() / b.as_secs_f64();
        if ratio <= cap {
            return;
        }
        over.push(format!("{a:?} against {b:?} ({ratio:.2})"));
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
