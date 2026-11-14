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

/// A fan-in of `senders` tasks that each send three numbers to one
/// channel of sixteen places, where most of them wait their turn; the
/// program prints the sum of what arrived.
fn fan_in_program(senders: usize) -> String {
    format!(
        "import channel
import list
import task

fn fan_in(senders) {{
  let ch = channel.new(16)
  let hs = 1..senders |> list.map {{ i -> task.spawn({{ ->
    1..3 |> list.each {{ j -> channel.send(ch, i + j) }}
  }}) }}
  let sum = loop left = senders * 3, acc = 0 {{
    match left {{
      0 -> acc
      _ -> match channel.receive(ch) {{
        Some(v) -> loop(left - 1, acc + v)
        None -> acc
      }}
    }}
  }}
  hs |> list.each {{ h -> task.join(h) }}
  sum
}}

fn main() {{
  println(fan_in({senders}))
}}
"
    )
}

/// Many tasks parked on one channel cost time in proportion to their
/// number, not to its square: with four times the senders the fan-in
/// takes about four times as long, at most eight times. (When leaving
/// the channel's queue meant searching it, four times the senders took
/// sixteen times as long.)
#[test]
fn parked_senders_cost_time_in_proportion() {
    let programs = Programs::new("fan_in");
    let large = programs.file("large.silt", &fan_in_program(20_000));
    let small = programs.file("small.silt", &fan_in_program(5_000));
    assert_within(
        8.0,
        (&large, "600150000"),
        (&small, "37537500"),
        "a fan-in of 20,000 senders, against one of 5,000",
    );
}

/// A loop of 125,000 rounds that calls the method `method` of a record
/// eight times a round, a million calls. The method is of a trait and
/// is found where the code runs: the function that calls it knows its
/// argument only as a type with the trait. `other_impls` other types
/// implement the trait too, each declared before the record's type.
fn million_method_calls_program(method: &str, other_impls: usize) -> String {
    let others: String = (0..other_impls)
        .map(|i| {
            format!(
                "type Other{i} {{
  n: Int,
}}

trait Size for Other{i} {{
  fn {method}(self) -> Int {{
    self.n + {i}
  }}
}}

"
            )
        })
        .collect();
    let call = format!("Size.{method}(x)");
    let calls = [call.as_str(); 8].join(" + ");
    format!(
        "trait Size {{
  fn {method}(self) -> Int
}}

{others}type Box {{
  n: Int,
}}

trait Size for Box {{
  fn {method}(self) -> Int {{
    self.n
  }}
}}

fn total(x: a, times: Int) -> Int where a: Size {{
  loop i = 0, acc = 0 {{
    match i >= times {{
      true -> acc
      false -> loop(i + 1, acc + {calls})
    }}
  }}
}}

fn main() {{
  println(total(Box {{ n: 2 }}, 125000))
}}
"
    )
}

/// A method that is found where the code runs is found by three
/// numbers in one table: the trait, the receiver's type, and the id of
/// the method's name. So what a call costs does not depend on the
/// name: a million calls of a method whose name has 10,000 letters
/// take as long as a million of a method named `s`, at most three
/// times as long. The method was found by its name, a text that was
/// hashed and compared at every call: the calls of the long name took
/// eight times as long.
#[test]
fn a_method_call_costs_the_same_whatever_the_length_of_its_name() {
    let programs = Programs::new("method_names");
    let long = programs.file(
        "long.silt",
        &million_method_calls_program(&"s".repeat(10_000), 0),
    );
    let short = programs.file("short.silt", &million_method_calls_program("s", 0));
    assert_within(
        3.0,
        (&long, "2000000"),
        (&short, "2000000"),
        "a million calls of a method with a name of 10,000 letters, \
         against a million of a method named `s`",
    );
}

/// Nor does it depend on how many types implement the trait: a million
/// calls take as long in a program where 200 other types implement it
/// as where the receiver's type is the only one, at most three times
/// as long.
#[test]
fn a_method_call_costs_the_same_however_many_types_implement_the_trait() {
    let programs = Programs::new("method_impls");
    let many = programs.file("many.silt", &million_method_calls_program("size", 200));
    let one = programs.file("one.silt", &million_method_calls_program("size", 0));
    assert_within(
        3.0,
        (&many, "2000000"),
        (&one, "2000000"),
        "a million calls of a method of a trait that 201 types implement, \
         against a million where one type does",
    );
}

/// A program of `functions` functions whose `main` starts `tasks`
/// tasks that each wait for a message nobody sends, prints their
/// number when every one of them runs, and waits for a line of input.
#[cfg(target_os = "linux")]
fn waiting_tasks_program(functions: usize, tasks: usize) -> String {
    let functions: String = (0..functions)
        .map(|i| format!("fn g{i}(x: Int) -> Int {{\n  x + {i}\n}}\n\n"))
        .collect();
    format!(
        "import channel
import io
import list
import task

{functions}fn main() {{
  let never = channel.new(0)
  let running = channel.new({tasks})
  let handles = list.map(1..{tasks}) {{ i ->
    task.spawn({{ ->
      let _ = channel.send(running, i)
      match channel.receive(never) {{
        Some(n) -> n + g0(i)
        None -> 0
      }}
    }})
  }}
  list.each(handles) {{ _ ->
    let _ = channel.receive(running)
  }}
  println(list.length(handles))
  let _ = io.read_line()
}}
"
    )
}

/// A task is started with nothing of the program copied: the VMs of
/// all tasks have the program's global slots in common. Each had a
/// copy of them, so 5,000 waiting tasks of a program of 2,000
/// functions held 400 MB; they hold some 60 MB in a local debug
/// build, and 97 to 105 MB on the CI runners. The limit of 200 MB is
/// half of what a copy per task costs.
///
/// What is measured is the most memory the program has had resident
/// while its tasks wait (`VmHWM` of its process, which Linux gives).
#[test]
#[cfg(target_os = "linux")]
fn five_thousand_waiting_tasks_share_the_programs_globals() {
    use std::io::{BufRead, BufReader, Write};

    let programs = Programs::new("tasks");
    let file = programs.file("tasks.silt", &waiting_tasks_program(2_000, 5_000));
    let mut silt = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("run")
        .arg(&file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("silt runs");
    // Every task runs, and waits or is about to.
    let mut line = String::new();
    BufReader::new(silt.stdout.as_mut().expect("the program's output"))
        .read_line(&mut line)
        .expect("the program's first line");
    let status = std::fs::read_to_string(format!("/proc/{}/status", silt.id()))
        .expect("the status of the program's process");
    // (The program ends when it has its line.)
    let mut stdin = silt.stdin.take().expect("the program's input");
    stdin.write_all(b"\n").expect("a line for the program");
    drop(stdin);
    let output = silt.wait_with_output().expect("silt's output");
    assert_eq!(
        line.trim(),
        "5000",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let resident: u64 = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|kb| kb.trim().trim_end_matches("kB").trim().parse().ok())
        .expect("VmHWM, in kB");
    assert!(
        resident < 200 * 1024,
        "5,000 waiting tasks of a program of 2,000 functions held {resident} kB"
    );
}

/// A program that calls host functions `calls` times with an argument
/// of `size` elements, 2 or `big`: a lookup whose list holds no value to
/// show the variable but at its end (`List(Option(a))` of `None`s), and
/// a function that gives back a list of `size` empty lists. The program
/// makes the lists of both sizes whichever it passes, and the loops
/// around the calls are the same: two runs differ in the size of the
/// arguments alone, and their ratio does not hang on what the rest of
/// the program costs (a faster `match` made the run over 2 elements
/// faster by more than the other, and the ratio went from 2.8 to 3.1).
fn host_calls_program(size: usize, big: usize, calls: usize) -> String {
    let picked = match size == big {
        true => "big",
        false => "small",
    };
    format!(
        "import h
import list

fn main() {{
  let small = (list.append((1..2) |> list.map({{ _ -> None }}), Some(1)), (1..2) |> list.map({{ _ -> [] }}))
  let big = (list.append((1..{big}) |> list.map({{ _ -> None }}), Some(1)), (1..{big}) |> list.map({{ _ -> [] }}))
  let (xs, ls) = {picked}
  let hits = (1..{calls}) |> list.fold(0) {{ acc, _ ->
    match h.getopt(xs, {size}) {{
      Some(n) -> acc + n
      None -> acc
    }}
  }}
  let lengths = (1..{calls}) |> list.fold(0) {{ acc, _ -> acc + list.length(h.same(ls)) }}
  host.give(hits + lengths)
}}
import host
"
    )
}

/// How long a run of `source` with the host module of
/// `host_calls_program` takes; it must give `expected`.
fn host_run_time(source: &str, expected: usize) -> Duration {
    use silt::session::{HostCall, HostModule};
    use silt::value::Value;
    let host = HostModule::new("h")
        .function(
            "fn getopt(xs: List(Option(a)), i: Int) -> Option(a)",
            |call: &HostCall| {
                let (Value::List(xs), Value::Int(i)) = (&call.args[0], &call.args[1]) else {
                    return Err(silt::vm::VmError::new(
                        "expected a list and an index".into(),
                    ));
                };
                Ok(xs.get(*i as usize).unwrap_or(Value::Unit))
            },
        )
        .function(
            "fn same(xs: List(List(a))) -> List(List(a))",
            |call: &HostCall| Ok(call.args[0].clone()),
        );
    let started = Instant::now();
    let given = silt::session::testing::run_with_host(&[("main.silt", source)], vec![host]);
    let elapsed = started.elapsed();
    assert_eq!(given, Ok(Value::Int(expected as i64)));
    elapsed
}

/// What a call of a host function costs to check does not grow with the
/// size of its arguments or its result: the search for the value that
/// shows a variable and the check of the result each look at a bounded
/// number of parts. Each walked the whole argument and the whole
/// result, so 16,000 lookups into a list of 16,000 elements took 3.8
/// seconds where they take 10 ms (release builds), and calls over a
/// list grew as its square.
#[test]
fn a_host_function_call_costs_the_same_whatever_the_size_of_its_argument() {
    let (calls, size, cap) = (2_000, 2_000, 3.0);
    let big = host_calls_program(size, size, calls);
    let small = host_calls_program(2, size, calls);
    let mut over = Vec::new();
    for _ in 0..3 {
        let mut best = (Duration::MAX, Duration::MAX);
        for _ in 0..3 {
            best.1 = best.1.min(host_run_time(&small, calls * 3));
            best.0 = best.0.min(host_run_time(&big, calls * (size + 1)));
        }
        let ratio = best.0.as_secs_f64() / best.1.as_secs_f64();
        if ratio <= cap {
            return;
        }
        over.push(format!("{:?} against {:?} ({ratio:.2})", best.0, best.1));
    }
    panic!(
        "{calls} host calls over {size} elements: more than {cap} times as long as over 2 in each \
         of three measurements ({})",
        over.join(", ")
    );
}

/// `parses` calls of `time.parse`, every `every`-th Int from 1 to
/// `parses * every` reading a text by a pattern of its own (`fresh`)
/// or each by the same one.
fn parses_program(parses: usize, every: usize, fresh: bool) -> String {
    let tail = match fresh {
        true => "{i}",
        false => "7",
    };
    format!(
        "import list
import time

fn main() {{
  let n = list.fold(1..{to}, 0) {{ acc, i ->
    match i % {every} == 0 {{
      true -> match time.parse(\"2026-10-30T22:01:42 {tail}\", \"%Y-%m-%dT%H:%M:%S {tail}\") {{
        Ok(_) -> acc + 1
        Err(_) -> acc
      }}
      false -> acc
    }}
  }}
  println(n)
}}
",
        to = parses * every,
    )
}

/// A parse of a time by a pattern it was given before reads the text
/// and does not ask again whether the pattern is one of a `DateTime`
/// and has fields enough for one, which formats a moment by it item by
/// item and reads it back: that took three quarters of each parse, so
/// 200,000 parses by one pattern took five times as long as 20,000 by
/// a new pattern each, where they take one and a half (debug and
/// release builds alike; in a release build 2.5 us a parse, now 0.6).
#[test]
fn a_parse_by_a_pattern_used_before_does_not_check_it_again() {
    let programs = Programs::new("parse");
    let same = programs.file("same.silt", &parses_program(200_000, 1, false));
    let fresh = programs.file("fresh.silt", &parses_program(20_000, 10, true));
    assert_within(
        3.0,
        (&same, "200000"),
        (&fresh, "20000"),
        "200,000 parses of a time by one pattern, against 20,000 by a new pattern each",
    );
}
