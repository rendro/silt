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
//! made 2,000 of them take 28 s where the same module with each call
//! bound by `let _ =` took 0.35 s.

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

/// A top-level channel and `n` functions that each have the statement
/// `call`. With `take(ch)` every one of the calls waits, until the end
/// of the module, for the type `main` gives the channel; with
/// `let _ = take(ch)` none does.
fn calls_in_functions(n: usize, call: &str) -> String {
    let mut source = format!("{TAKE}let ch = channel.new(1)\n\n");
    for i in 0..n {
        source.push_str(&format!("fn f{i}() {{\n  {call}\n  {i}\n}}\n\n"));
    }
    source.push_str("fn main() {\n  channel.send(ch, ())\n  println(f0())\n}\n");
    source
}

/// One function with `n` statements `call`, each followed by a closure
/// bound with `let`: with `take(ch)` every closure is generalised while
/// all the calls before it still wait.
fn calls_between_closures(n: usize, call: &str) -> String {
    let mut source = format!("{TAKE}fn main() {{\n  let ch = channel.new(1)\n");
    for i in 0..n {
        source.push_str(&format!("  {call}\n  let h{i} = {{ x -> x }}\n"));
    }
    source.push_str("  channel.send(ch, ())\n  println(\"done\")\n}\n");
    source
}

/// The best check times of `module` with 2,000 calls that wait and with
/// 2,000 calls that do not (each bound by `let _ =`), measured in turn.
fn waiting_and_bound(name: &str, module: fn(usize, &str) -> String) -> (Duration, Duration) {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let waiting = dir.join("waiting.silt");
    let bound = dir.join("bound.silt");
    std::fs::write(&waiting, module(2_000, "take(ch)")).expect("the module is written");
    std::fs::write(&bound, module(2_000, "let _ = take(ch)")).expect("the module is written");
    // The best of several runs of each, in turn: the machine may be busy.
    let mut best_waiting = Duration::MAX;
    let mut best_bound = Duration::MAX;
    for _ in 0..7 {
        best_waiting = best_waiting.min(check_time(&waiting));
        best_bound = best_bound.min(check_time(&bound));
    }
    let _ = std::fs::remove_dir_all(&dir);
    (best_waiting, best_bound)
}

/// A call that waits for its type costs about what a call that does not
/// wait costs: a module of 2,000 of them checks in at most twice the
/// time of the same module with each call bound by `let _ =`.
///
/// The two modules are the same size, so the comparison does not depend
/// on how the checker's time grows with a module's size on the machine
/// at hand (on these modules it grows faster than linearly beyond a
/// thousand definitions, whatever the calls are, and by how much differs
/// between machines: a ratio between two sizes measures that too). A
/// cost per waiting call that grows with their number shows here as
/// soon as it doubles the check: reading every waiting call at every
/// generalisation made this ratio 80 (functions) and 47 (closures).
#[test]
fn statement_calls_that_wait_for_a_type_cost_the_same_each() {
    let shapes: [(&str, fn(usize, &str) -> String); 2] = [
        ("functions", calls_in_functions),
        ("closures", calls_between_closures),
    ];
    for (name, module) in shapes {
        let (waiting, bound) = waiting_and_bound(name, module);
        let ratio = waiting.as_secs_f64() / bound.as_secs_f64();
        assert!(
            ratio <= 2.0,
            "`silt check` of 2,000 statement calls that wait for a type ({name}) took \
             {waiting:?}, {ratio:.2} times the {bound:?} of the same calls bound by \
             `let _ =`: the calls that wait are read again and again"
        );
    }
}
