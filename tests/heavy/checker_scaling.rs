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
//! And in the number of a program's modules: the session's tables note
//! which rows a module's check enters as they are entered, and an impl
//! is validated by the module that writes it. Before, every module's
//! check took two snapshots of every key of every table and validated
//! every impl of the session again, so 4,000 small modules took 100 s
//! in a debug build where 1,000 took 7 s and 250 took 0.7 s.
//!
//! The same for the calls that stand as statements and wait for a type
//! (the unused-value rule): each waits under the scope that decides it
//! and is read when that scope ends, not at every generalisation, which
//! made 2,000 of them take 28 s where the same module with each call
//! bound by `let _ =` took 0.35 s.

//!
//! And in the number of a module's top-level `let`s (the compiler looked
//! up each one's place in the initialisation order by reading the order:
//! 20,000 took nine times as long as 5,000 in a debug build), and in the
//! number of uses of one binding whose type is still open (each use
//! added a link to a chain of type variables that every later use
//! walked: 8,000 functions that read one channel took eleven times as
//! long as 2,000).

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

/// The best check times of `first` and of `second`, `runs` runs of each,
/// in turn: the machine may be busy.
fn best_times(first: &Path, second: &Path, runs: usize) -> (Duration, Duration) {
    let mut best_first = Duration::MAX;
    let mut best_second = Duration::MAX;
    for _ in 0..runs {
        best_first = best_first.min(check_time(first));
        best_second = best_second.min(check_time(second));
    }
    (best_first, best_second)
}

/// `first` over `second`.
fn ratio((first, second): (Duration, Duration)) -> f64 {
    first.as_secs_f64() / second.as_secs_f64()
}

/// Measure two times with `measure` until the first is at most `cap`
/// times the second, at most three times. A machine that was busy
/// during one measurement is measured again; a ratio that is over the
/// cap because of what the checker does is over it every time. Returns
/// the measurements if all three were over, for the failure message.
fn within(
    cap: f64,
    mut measure: impl FnMut() -> (Duration, Duration),
) -> Result<(), Vec<(Duration, Duration)>> {
    let mut over = Vec::new();
    for _ in 0..3 {
        let measured = measure();
        if ratio(measured) <= cap {
            return Ok(());
        }
        over.push(measured);
    }
    Err(over)
}

/// The measurements of a failed `within`, for a message: each pair of
/// times with its ratio.
fn shown(over: &[(Duration, Duration)]) -> String {
    let each: Vec<String> = over
        .iter()
        .map(|&(first, second)| {
            format!(
                "{first:?} against {second:?} ({:.2})",
                ratio((first, second))
            )
        })
        .collect();
    each.join(", ")
}

#[test]
fn checking_4000_functions_takes_about_twice_as_long_as_2000() {
    let dir = std::env::temp_dir().join(format!("silt_checker_scaling_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let small = dir.join("small.silt");
    let large = dir.join("large.silt");
    std::fs::write(&small, module_of(2_000)).expect("the small module is written");
    std::fs::write(&large, module_of(4_000)).expect("the large module is written");
    let measured = within(2.3, || best_times(&large, &small, 5));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of 4,000 functions took more than 2.3 times as long as that of \
             2,000 functions in each of three measurements ({}): it is no longer linear in \
             the number of definitions",
            shown(&over)
        );
    }
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

/// A call that waits for its type costs about what a call that does not
/// wait costs: a module of 2,000 of them checks in at most twice the
/// time of the same module with each call bound by `let _ =`.
///
/// The two modules are the same size, so the comparison does not depend
/// on how the checker's time grows with a module's size on the machine
/// at hand (a ratio between two sizes measures that too:
/// `four_times_the_uses_of_an_open_binding_take_about_four_times_as_long`
/// is that test). A cost per waiting call that grows with their number
/// shows here as soon as it doubles the check: reading every waiting
/// call at every generalisation made this ratio 80 (functions) and 47
/// (closures).
#[test]
fn statement_calls_that_wait_for_a_type_cost_the_same_each() {
    let shapes: [(&str, fn(usize, &str) -> String); 2] = [
        ("functions", calls_in_functions),
        ("closures", calls_between_closures),
    ];
    for (name, module) in shapes {
        let dir = std::env::temp_dir().join(format!(
            "silt_checker_scaling_{name}_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let waiting = dir.join("waiting.silt");
        let bound = dir.join("bound.silt");
        std::fs::write(&waiting, module(2_000, "take(ch)")).expect("the module is written");
        std::fs::write(&bound, module(2_000, "let _ = take(ch)")).expect("the module is written");
        let measured = within(2.0, || best_times(&waiting, &bound, 3));
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(over) = measured {
            panic!(
                "`silt check` of 2,000 statement calls that wait for a type ({name}) took \
                 more than twice as long as that of the same calls bound by `let _ =` in \
                 each of three measurements ({}): the calls that wait are read again and \
                 again",
                shown(&over)
            );
        }
    }
}

/// `small` and `large` are written, measured (`large` against `small`,
/// at most `cap` times as long) and removed; what the measurements were
/// when all three were over the cap.
fn grows_within(
    name: &str,
    small: String,
    large: String,
    cap: f64,
) -> Result<(), Vec<(Duration, Duration)>> {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_{name}_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let (small_file, large_file) = (dir.join("small.silt"), dir.join("large.silt"));
    std::fs::write(&small_file, small).expect("the small module is written");
    std::fs::write(&large_file, large).expect("the large module is written");
    let measured = within(cap, || best_times(&large_file, &small_file, 3));
    let _ = std::fs::remove_dir_all(&dir);
    measured
}

/// A module of `n` top-level `let`s and a `main` that reads one.
fn lets_of(n: usize) -> String {
    let mut source = String::new();
    for i in 0..n {
        source.push_str(&format!("let v{i} = {i}\n"));
    }
    source.push_str("fn main() { println(v0) }\n");
    source
}

/// Four times the top-level `let`s take about four times as long, to
/// check and to compile: at most six times. (Looking up each `let`'s
/// place in the initialisation order by reading the order made it nine
/// times.)
#[test]
fn four_times_the_top_level_lets_take_about_four_times_as_long() {
    if let Err(over) = grows_within("lets", lets_of(5_000), lets_of(20_000), 6.0) {
        panic!(
            "`silt check` of 20,000 top-level lets took more than six times as long as \
             that of 5,000 in each of three measurements ({}): it is no longer linear in \
             the number of lets",
            shown(&over)
        );
    }
}

/// Four times the uses of one binding whose type is still open (a
/// top-level channel that `main` decides, read in every function) take
/// about four times as long: at most six times. (When the older of two
/// type variables was bound to the newer one, each use added a link to
/// a chain that every later use walked, and it was eleven times.)
#[test]
fn four_times_the_uses_of_an_open_binding_take_about_four_times_as_long() {
    let module = |n| calls_in_functions(n, "let _ = take(ch)");
    if let Err(over) = grows_within("uses", module(2_000), module(8_000), 6.0) {
        panic!(
            "`silt check` of 8,000 functions that read one channel took more than six \
             times as long as that of 2,000 in each of three measurements ({}): a use of \
             a binding whose type is open costs more the more uses there are",
            shown(&over)
        );
    }
}

/// A program of `n` small modules beside its `main.silt`, which imports
/// them all: each declares a type and two functions.
fn program_of_modules(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    let mut main = String::new();
    for i in 0..n {
        let module = format!(
            "pub type T{i} {{\n  n: Int,\n}}\n\n\
             pub fn make{i}(n: Int) -> T{i} {{\n  T{i} {{ n: n + {i} }}\n}}\n\n\
             pub fn f{i}(x: Int) -> Int {{\n  make{i}(x).n + {i}\n}}\n"
        );
        std::fs::write(dir.join(format!("m{i}.silt")), module).expect("a module is written");
        main.push_str(&format!("import m{i}\n"));
    }
    main.push_str("\nfn main() {\n  println(m0.f0(1))\n}\n");
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// Four times the modules take about four times as long to check, from
/// 250 to 1,000 and from 1,000 to 4,000: at most six times. (When every
/// module's check read the whole session's tables, 1,000 modules took
/// nine times as long as 250, and 4,000 fifteen times as long as 1,000.)
#[test]
fn checking_four_times_the_modules_takes_about_four_times_as_long() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_modules_{}",
        std::process::id()
    ));
    let sizes = [250, 1_000, 4_000];
    for n in sizes {
        program_of_modules(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured: Vec<_> = sizes
        .windows(2)
        .map(|pair| {
            let (small, large) = (pair[0], pair[1]);
            let measured = within(6.0, || best_times(&main_of(large), &main_of(small), 3));
            (small, large, measured)
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    for (small, large, measured) in measured {
        if let Err(over) = measured {
            panic!(
                "`silt check` of a program of {large} modules took more than six times as \
                 long as that of one of {small} in each of three measurements ({}): it is \
                 no longer linear in the number of modules",
                shown(&over)
            );
        }
    }
}
