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

/// A recursion `depth` deep, five times: `plain`, in which every
/// second call is a tail call (`hop` calls `plain` as its last act),
/// or `folded`, in which every call is made by a function `list.fold`
/// calls and is that function's last act.
fn deep_recursion_program(recursion: &str, depth: usize) -> String {
    format!(
        "import list

fn hop(n) {{
  plain(n)
}}

fn plain(n) {{
  match n {{
    0 -> 0
    _ -> 1 + hop(n - 1)
  }}
}}

fn folded(n) {{
  match n {{
    0 -> 0
    _ -> 1 + list.fold([n - 1], 0) {{ _, x -> folded(x) }}
  }}
}}

fn main() {{
  let depths = list.map(1..5) {{ _ -> {recursion}({depth}) }}
  println(list.sum(depths))
}}
"
    )
}

/// Assert that the recursion at twice the depth takes about twice as
/// long: at most three and a half times.
fn assert_deep_recursion_is_linear(recursion: &str) {
    let programs = Programs::new(recursion);
    let deep = programs.file("deep.silt", &deep_recursion_program(recursion, 80_000));
    let half = programs.file("half.silt", &deep_recursion_program(recursion, 40_000));
    assert_within(
        3.5,
        (&deep, "400000"),
        (&half, "200000"),
        &format!("`{recursion}` 80,000 calls deep, against 40,000"),
    );
}

/// A return costs the same however many tail calls the calls in
/// progress have made: a recursion in which every second call is a
/// tail call takes about twice as long when it is twice as deep. It
/// took four times as long when every return went through the record
/// of all the callers that tail calls had replaced.
#[test]
fn a_return_costs_the_same_however_many_tail_calls_were_made() {
    assert_deep_recursion_is_linear("plain");
}

/// The same for a function that `list.fold` calls and whose body is a
/// tail call.
#[test]
fn a_return_from_a_function_a_builtin_calls_costs_the_same_at_any_depth() {
    assert_deep_recursion_is_linear("folded");
}

/// Five times 2,000 parks of a task that has `depth` calls in
/// progress, each made by a function that `list.fold` calls: the task
/// asks another task for an answer, and waits for it.
fn parks_program(depth: usize) -> String {
    format!(
        "import channel
import list
import task

fn echo(asks, answers) {{
  channel.each(asks) {{ n -> channel.send(answers, n) }}
}}

fn parks(count, asks, answers) {{
  loop i = 0 {{
    match i >= count {{
      true -> i
      false -> {{
        channel.send(asks, i)
        let _ = channel.receive(answers)
        loop(i + 1)
      }}
    }}
  }}
}}

fn at_depth(n, asks, answers) {{
  match n {{
    0 -> parks(2000, asks, answers)
    _ -> list.fold([n], 0) {{ acc, x -> acc + at_depth(x - 1, asks, answers) }}
  }}
}}

fn round(n) {{
  let asks = channel.new(0)
  let answers = channel.new(0)
  let server = task.spawn({{ -> echo(asks, answers) }})
  let parked = task.join(task.spawn({{ -> at_depth(n, asks, answers) }}))
  channel.close(asks)
  task.join(server)
  parked
}}

fn main() {{
  println(list.fold(1..5, 0) {{ parked, _ -> parked + round({depth}) }})
}}
"
    )
}

/// A task that parks deep inside functions that builtins call goes on
/// where it was: the cost of a park does not grow with the depth.
/// 10,000 parks take about as long 900 calls deep as they do at the
/// top: at most three times. They took several times as long when
/// every park unwound and rebuilt the calls in progress.
#[test]
fn parking_deep_in_callbacks_costs_the_same_at_any_depth() {
    let programs = Programs::new("parks");
    let deep = programs.file("deep.silt", &parks_program(900));
    let top = programs.file("top.silt", &parks_program(0));
    assert_within(
        3.0,
        (&deep, "10000"),
        (&top, "10000"),
        "10,000 parks 900 calls deep, against 10,000 at the top",
    );
}
