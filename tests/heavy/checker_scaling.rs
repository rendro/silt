//! `silt check` is linear in the number of a module's top-level
//! definitions.
//!
//! Before stage 6 the checker generalised each function by scanning the
//! whole environment and copied the module's scope for every body, so
//! 2,000 one-line functions took four times as long as 1,000 (13 s in a
//! debug build). A function is now generalised by the levels of its own
//! type variables and its body is checked in a frame pushed on the one
//! environment; the parser finds a declaration's line in a table.
//!
//! The same for the calls that stand as statements and wait for a type
//! (the unused-value rule): each waits under the scope that decides it
//! and is read when that scope ends, not at every generalisation, which
//! made 2,000 of them take 28 s where 1,000 took 4 s (seven times as
//! long; the test allows three).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// A module of `n` one-line functions and `n / 10` top-level `let`s.
/// Most functions stand alone; every fourth calls the one before it, and
/// every tenth calls the one after it, so the checker orders them.
fn module_of(n: usize) -> String {
    let mut source = String::new();
    for i in 0..n {
        let line = if i % 10 == 9 && i + 1 < n {
            format!("fn f{i}(x) {{ f{}(x) + {i} }}\n", i + 1)
        } else if i % 4 == 3 {
            format!("fn f{i}(x) {{ f{}(x) + {i} }}\n", i - 1)
        } else {
            format!("fn f{i}(x) {{ x + {i} }}\n")
        };
        source.push_str(&line);
        if i % 10 == 0 {
            source.push_str(&format!("let v{i} = f{i}({i})\n"));
        }
    }
    source.push_str("fn main() { println(f0(v0)) }\n");
    source
}

/// How long `silt check` of the file takes; it must find nothing.
fn check_time(file: &Path) -> Duration {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(file)
        .output()
        .expect("silt runs");
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "the module checks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

#[test]
fn checking_4000_functions_takes_about_twice_as_long_as_2000() {
    let dir = std::env::temp_dir().join(format!("silt_checker_scaling_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let small = dir.join("small.silt");
    let large = dir.join("large.silt");
    std::fs::write(&small, module_of(2_000)).expect("the small module is written");
    std::fs::write(&large, module_of(4_000)).expect("the large module is written");
    // The best of several runs of each, in turn: the machine may be busy.
    let mut best_small = Duration::MAX;
    let mut best_large = Duration::MAX;
    for _ in 0..7 {
        best_small = best_small.min(check_time(&small));
        best_large = best_large.min(check_time(&large));
    }
    let _ = std::fs::remove_dir_all(&dir);
    let ratio = best_large.as_secs_f64() / best_small.as_secs_f64();
    assert!(
        ratio <= 2.3,
        "`silt check` of 4,000 functions took {best_large:?}, {ratio:.2} times the \
         {best_small:?} of 2,000 functions: it is no longer linear in the number of \
         definitions"
    );
}

/// What the statement-call modules start with: `take` returns what the
/// channel carries, so `take(ch)` as a statement is a call whose type
/// waits for the channel's.
const TAKE: &str = "import channel\nimport channel.{ Message }\n\n\
    fn take(c: Channel(a)) -> a {\n  match channel.receive(c) {\n    \
    Message(v) -> v\n    _ -> panic(\"closed\")\n  }\n}\n\n";

/// A top-level channel and `n` functions that each call `take(ch)` as a
/// statement: every one of the calls waits, until the end of the
/// module, for the type `main` gives the channel.
fn waiting_calls_in_functions(n: usize) -> String {
    let mut source = format!("{TAKE}let ch = channel.new(1)\n\n");
    for i in 0..n {
        source.push_str(&format!("fn f{i}() {{\n  take(ch)\n  {i}\n}}\n\n"));
    }
    source.push_str("fn main() {\n  channel.send(ch, ())\n  println(f0())\n}\n");
    source
}

/// One function with `n` statements `take(ch)`, each followed by a
/// closure bound with `let`: every closure is generalised while all the
/// calls before it still wait.
fn waiting_calls_between_closures(n: usize) -> String {
    let mut source = format!("{TAKE}fn main() {{\n  let ch = channel.new(1)\n");
    for i in 0..n {
        source.push_str(&format!("  take(ch)\n  let h{i} = {{ x -> x }}\n"));
    }
    source.push_str("  channel.send(ch, ())\n  println(\"done\")\n}\n");
    source
}

/// The ratio of the best check times of `module(2 * n)` and `module(n)`.
fn doubling_ratio(name: &str, n: usize, module: fn(usize) -> String) -> (f64, Duration, Duration) {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let small = dir.join("small.silt");
    let large = dir.join("large.silt");
    std::fs::write(&small, module(n)).expect("the small module is written");
    std::fs::write(&large, module(2 * n)).expect("the large module is written");
    let mut best_small = Duration::MAX;
    let mut best_large = Duration::MAX;
    for _ in 0..5 {
        best_small = best_small.min(check_time(&small));
        best_large = best_large.min(check_time(&large));
    }
    let _ = std::fs::remove_dir_all(&dir);
    (
        best_large.as_secs_f64() / best_small.as_secs_f64(),
        best_small,
        best_large,
    )
}

#[test]
fn statement_calls_that_wait_for_a_type_cost_the_same_each() {
    let shapes: [(&str, fn(usize) -> String); 2] = [
        ("functions", waiting_calls_in_functions),
        ("closures", waiting_calls_between_closures),
    ];
    for (name, module) in shapes {
        let (ratio, small, large) = doubling_ratio(name, 1_000, module);
        assert!(
            ratio <= 3.0,
            "`silt check` of 2,000 waiting statement calls ({name}) took {large:?}, \
             {ratio:.2} times the {small:?} of 1,000: the calls that wait are read \
             again and again"
        );
    }
}
