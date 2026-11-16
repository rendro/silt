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
//! long as 2,000). And in the number of warnings, each kept once by a
//! set beside the list of them. And in the number of impls of one trait
//! for one type: each new impl was unified with every one before it, and
//! each use of a trait without arguments with every impl.

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

/// 8,000 functions against 4,000, not 4,000 against 2,000: the time a
/// check takes to start is the same at both sizes and hides some of
/// what grows. A lookup that read the whole module once per function
/// made 4,000 take 2.07-2.16 times as long as 2,000 in a debug build on
/// Linux, under the cap, and 8,000 take 2.43-2.55 times as long as
/// 4,000; read by name, the second ratio is 1.86-2.00.
#[test]
fn checking_8000_functions_takes_about_twice_as_long_as_4000() {
    let dir = std::env::temp_dir().join(format!("silt_checker_scaling_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let small = dir.join("small.silt");
    let large = dir.join("large.silt");
    std::fs::write(&small, module_of(4_000)).expect("the small module is written");
    std::fs::write(&large, module_of(8_000)).expect("the large module is written");
    let measured = within(2.3, || best_times(&large, &small, 5));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of 8,000 functions took more than 2.3 times as long as that of \
             4,000 functions in each of three measurements ({}): it is no longer linear in \
             the number of definitions",
            shown(&over)
        );
    }
}

/// What the statement-call modules start with: `take` returns what the
/// channel carries, so `take(ch)` as a statement is a call whose type
/// waits for the channel's.
const TAKE: &str = "import channel\n\n\
    fn take(c: Channel(a)) -> a {\n  match channel.receive(c) {\n    \
    Some(v) -> v\n    None -> panic(\"closed\")\n  }\n}\n\n";

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

/// A module of `n` functions, each with a `let` that nothing reads: `n`
/// warnings.
fn unused_lets_of(n: usize) -> String {
    let mut source = String::new();
    for i in 0..n {
        source.push_str(&format!(
            "fn f{i}() -> Int {{\n  let unused{i} = {i}\n  {i}\n}}\n\n"
        ));
    }
    source.push_str("fn main() { println(f0()) }\n");
    source
}

/// Four times the warnings take about four times as long: at most six
/// times. (Keeping each diagnostic once by looking through the list of
/// those kept made it quadratic: limits_wrap_locals.silt, a module of
/// many unused names, took 1.9 s to check instead of 0.1 s.)
#[test]
fn four_times_the_warnings_take_about_four_times_as_long() {
    if let Err(over) = grows_within(
        "warnings",
        unused_lets_of(2_000),
        unused_lets_of(8_000),
        6.0,
    ) {
        panic!(
            "`silt check` of 8,000 unused names took more than six times as long as that \
             of 2,000 in each of three measurements ({}): keeping each diagnostic once is \
             no longer linear",
            shown(&over)
        );
    }
}

/// A module of `n` record types and `n` impls of one trait for `String`,
/// told apart by the trait argument (`trait Conv(T5) for String`), with
/// a use of each.
fn impls_by_trait_argument(n: usize) -> String {
    let mut source = String::from("trait Conv(b) {\n  fn conv(self) -> b\n}\n\n");
    for i in 0..n {
        source.push_str(&format!("type T{i} {{ v: Int }}\n"));
    }
    for i in 0..n {
        source.push_str(&format!(
            "\ntrait Conv(T{i}) for String {{\n  fn conv(self) -> T{i} {{\n    T{i} {{ v: {i} }}\n  }}\n}}\n"
        ));
    }
    source.push_str("\nfn main() {\n  let total = 0\n");
    for i in 0..n {
        source.push_str(&format!(
            "  let a{i}: T{i} = Conv.conv(\"x\")\n  let total = total + a{i}.v\n"
        ));
    }
    source.push_str("  println(total)\n}\n");
    source
}

/// A module of `n` record types and `n` impls of a trait without
/// arguments, each for an instantiation of one type (`trait Name for
/// Box(T5)`), with a use of each.
fn impls_by_instantiation(n: usize) -> String {
    let mut source =
        String::from("trait Name {\n  fn name(self) -> Int\n}\n\ntype Box(a) { v: a }\n\n");
    for i in 0..n {
        source.push_str(&format!("type T{i} {{ v: Int }}\n"));
    }
    for i in 0..n {
        source.push_str(&format!(
            "\ntrait Name for Box(T{i}) {{\n  fn name(self) -> Int {{\n    {i}\n  }}\n}}\n"
        ));
    }
    source.push_str("\nfn main() {\n  let total = 0\n");
    for i in 0..n {
        source.push_str(&format!(
            "  let total = total + Name.name(Box {{ v: T{i} {{ v: 1 }} }})\n"
        ));
    }
    source.push_str("  println(total)\n}\n");
    source
}

/// Four times the impls of one trait for one type take about four times
/// as long: at most six times. Impls told apart by their trait arguments
/// (`Conv(T5)`), and impls for instantiations of the type (`Box(T5)`).
/// (Each impl was unified with every impl written before it, to find one
/// that a use could mean as well: 2,048 of the first kind took 7.6 s in
/// a debug build where 1,024 took 2.1 s. And each use of the second kind
/// was unified with every impl: 2,048 took 24 s where 1,024 took 5.7 s.
/// Now an impl, and a use, is unified only with the impls whose first
/// trait argument, or first part of the type, could be its own by its
/// outer type.)
#[test]
fn four_times_the_impls_of_a_trait_for_a_type_take_about_four_times_as_long() {
    let shapes: [(&str, fn(usize) -> String); 2] = [
        ("trait arguments", impls_by_trait_argument),
        ("instantiations", impls_by_instantiation),
    ];
    for (name, module) in shapes {
        if let Err(over) = grows_within("impls", module(512), module(2_048), 6.0) {
            panic!(
                "`silt check` of 2,048 impls of one trait for one type, told apart by their \
                 {name}, took more than six times as long as that of 512 in each of three \
                 measurements ({}): an impl or a use is compared with every impl",
                shown(&over)
            );
        }
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

/// A program of `n` modules in a chain beside its `main.silt`: `m0`
/// imports `m1`, which imports `m2`, and so on, each with a type and a
/// function that calls the next one's.
fn program_of_a_chain(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    for i in 0..n {
        let (import, call) = match i + 1 < n {
            true => (format!("import m{}\n", i + 1), format!("m{}.f()", i + 1)),
            false => (String::new(), "1".to_string()),
        };
        let module =
            format!("{import}pub type T{i} {{ v: Int }}\npub fn f() -> Int {{ {call} }}\n");
        std::fs::write(dir.join(format!("m{i}.silt")), module).expect("a module is written");
    }
    let main = "import m0\n\nfn main() {\n  println(m0.f())\n}\n";
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// A chain of eight times the modules takes at most ten times as long to
/// check, from 250 to 2,000: about six and a half times (a fixed cost of
/// each run is in both). Every module's check used to walk all the
/// modules behind it for its reach (sixteen to eighteen times, main
/// d732e072), and also to work out how it names each of them for the
/// editor (twenty-five to thirty times, 5c053c37); it now walks them only
/// where a check asks.
#[test]
fn a_chain_of_eight_times_the_modules_takes_at_most_ten_times_as_long() {
    let dir =
        std::env::temp_dir().join(format!("silt_checker_scaling_chain_{}", std::process::id()));
    let (small, large) = (250, 2_000);
    for n in [small, large] {
        program_of_a_chain(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured = within(10.0, || best_times(&main_of(large), &main_of(small), 3));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of a chain of {large} modules took more than ten times as long as \
             that of one of {small} in each of three measurements ({}): a module's check walks \
             every module it reaches",
            shown(&over)
        );
    }
}

/// A program of `n` modules in a chain beside its `main.silt`, where each
/// module declares a trait with an impl and calls the trait method of
/// the next module's: `m0` calls `m1.Heavy1.weight1(m1.mk())`, and so on.
fn program_of_a_trait_chain(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    for i in 0..n {
        let (import, call) = match i + 1 < n {
            true => {
                let j = i + 1;
                (
                    format!("import m{j}\n"),
                    format!("m{j}.Heavy{j}.weight{j}(m{j}.mk())"),
                )
            }
            false => (String::new(), "1".to_string()),
        };
        let module = format!(
            "{import}pub type T {{ v: Int }}\n\
             pub trait Heavy{i} {{ fn weight{i}(self) -> Int }}\n\
             trait Heavy{i} for T {{ fn weight{i}(self) -> Int {{ self.v }} }}\n\
             pub fn mk() -> T {{ T {{ v: 1 }} }}\n\
             pub fn f() -> Int {{ {call} }}\n"
        );
        std::fs::write(dir.join(format!("m{i}.silt")), module).expect("a module is written");
    }
    let main = "import m0\n\nfn main() {\n  println(m0.f())\n}\n";
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// A chain of eight times the modules that call each other's trait
/// methods takes at most sixteen times as long to check, from 500 to
/// 4,000: about nine and a half times by instructions (debug build; 8 to
/// 8.5 by CPU time here, 11.4 to 11.8 by wall time on a macOS runner of
/// CI, where the cap was eleven). Each module's check looked at every
/// trait and every impl of the session (for its own traits, and for the
/// traits whose impls ask something of what they are given): 28 times
/// by instructions (rv-zc N18); now only at its own and at those it
/// calls.
#[test]
fn a_chain_of_trait_calls_of_eight_times_the_modules_takes_at_most_sixteen_times_as_long() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_trait_chain_{}",
        std::process::id()
    ));
    let (small, large) = (500, 4_000);
    for n in [small, large] {
        program_of_a_trait_chain(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured = within(16.0, || best_times(&main_of(large), &main_of(small), 3));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of a chain of {large} modules that call each other's trait methods \
             took more than sixteen times as long as that of one of {small} in each of three \
             measurements ({}): a module's check looks at every trait or impl of the session",
            shown(&over)
        );
    }
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

/// A program of `n` top-level `let`s beside its `main.silt`, each
/// reading the one before (`let a1 = a0 + 1`).
fn program_of_a_let_chain(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    let mut main = String::from("let a0 = 1\n");
    for i in 1..n {
        main.push_str(&format!("let a{i} = a{} + 1\n", i - 1));
    }
    main.push_str(&format!("\nfn main() {{\n  println(a{})\n}}\n", n - 1));
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// A chain of four times the top-level `let`s, each reading the one
/// before, takes at most six times as long to check, from 800 to 3,200:
/// about two and a half times (debug build). The order of the `let`s
/// kept, for each `let`, every `let` it needs, which in such a chain is
/// every `let` before it: fifteen times as long (main e6b2f85e: 2.7 s and
/// 40 s), and before that the ways to all of them and a search for a ring
/// from every `let` (n cubed, rv-a1b). Each `let` is now known by the
/// nearest `let`s it reaches; all it needs is found only where a ring is
/// searched for.
#[test]
fn a_chain_of_four_times_the_top_level_lets_takes_at_most_six_times_as_long() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_let_chain_{}",
        std::process::id()
    ));
    let (small, large) = (800, 3_200);
    for n in [small, large] {
        program_of_a_let_chain(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured = within(6.0, || best_times(&main_of(large), &main_of(small), 3));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of a chain of {large} top-level lets took more than six times as \
             long as that of one of {small} in each of three measurements ({}): the order of \
             the lets keeps or searches all each one needs for every let",
            shown(&over)
        );
    }
}

/// The most memory `silt check` of `file` had resident, in kB.
#[cfg(target_os = "linux")]
fn check_peak_kb(file: &Path) -> i64 {
    use std::process::Stdio;
    // (`wait4` below reaps it, for the memory it had.)
    #[allow(clippy::zombie_processes)]
    let child = Command::new(env!("CARGO_BIN_EXE_silt"))
        .arg("check")
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("silt runs");
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value of it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: the pid is of our child, not waited for yet; `status` and
    // `usage` are valid for writes.
    let waited = unsafe { libc::wait4(child.id() as libc::pid_t, &mut status, 0, &mut usage) };
    assert_eq!(waited, child.id() as libc::pid_t);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    usage.ru_maxrss
}

/// The chain of 800 top-level `let`s takes at most three times the
/// memory of the chain of 200 (about 1.2 times: the rest of the program
/// is the same). The ways to every `let` each one needs took twenty times
/// as much, a gigabyte (debug build, rv-a1b).
#[test]
#[cfg(target_os = "linux")]
fn a_chain_of_four_times_the_top_level_lets_takes_at_most_three_times_the_memory() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_let_memory_{}",
        std::process::id()
    ));
    let (small, large) = (200, 800);
    for n in [small, large] {
        program_of_a_let_chain(&dir.join(n.to_string()), n);
    }
    let peak = |n: usize| check_peak_kb(&dir.join(n.to_string()).join("main.silt"));
    let (at_small, at_large) = (peak(small), peak(large));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        at_large <= 3 * at_small,
        "`silt check` of a chain of {large} top-level lets had {at_large} kB resident, more than \
         three times the {at_small} kB of one of {small}: the order of the lets keeps all each \
         one needs for every let"
    );
}

/// A program of `n` top-level `let`s that each read the last of a chain
/// of `n / 2` aliases (`let p1 = p0`, a plain value each), beside its
/// `main.silt` (rv-lt2's hub).
fn program_of_a_let_hub(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    let aliases = n / 2;
    let mut main = String::from("let p0 = 1\n");
    for i in 1..aliases {
        main.push_str(&format!("let p{i} = p{}\n", i - 1));
    }
    for i in 0..n {
        main.push_str(&format!("let a{i} = p{} + {i}\n", aliases - 1));
    }
    main.push_str(&format!("\nfn main() {{\n  println(a{})\n}}\n", n - 1));
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// Four times the `let`s that each reach a chain of plain `let`s (the
/// hub above) take at most six times as long to check, from 800 to 3,200:
/// about three times (debug build). Each `let` kept every plain `let` on
/// its way: n squared, about thirteen times as long (main 96fda6f8: 0.7 s
/// for 800, 8.8 s and 196 MB for 3,200). A plain `let` is now passed by one node
/// that stands for what lies past it, searched once.
#[test]
fn a_hub_of_four_times_the_top_level_lets_takes_at_most_six_times_as_long() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_let_hub_{}",
        std::process::id()
    ));
    let (small, large) = (800, 3_200);
    for n in [small, large] {
        program_of_a_let_hub(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured = within(6.0, || best_times(&main_of(large), &main_of(small), 3));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of {large} top-level lets that each reach a chain of plain lets took \
             more than six times as long as that of {small} in each of three measurements ({}): \
             each let keeps every plain let on its way",
            shown(&over)
        );
    }
}

/// The hub of 3,200 `let`s takes at most three times the memory of the
/// hub of 800 (about 1.4 times). Each `let` keeping every plain `let` on
/// its way took more memory than main's before it (rv-lt2: 79 MB against
/// 67 MB at 1,600) and grows as n squared.
#[test]
#[cfg(target_os = "linux")]
fn a_hub_of_four_times_the_top_level_lets_takes_at_most_three_times_the_memory() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_let_hub_memory_{}",
        std::process::id()
    ));
    let (small, large) = (800, 3_200);
    for n in [small, large] {
        program_of_a_let_hub(&dir.join(n.to_string()), n);
    }
    let peak = |n: usize| check_peak_kb(&dir.join(n.to_string()).join("main.silt"));
    let (at_small, at_large) = (peak(small), peak(large));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        at_large <= 3 * at_small,
        "`silt check` of a hub of {large} top-level lets had {at_large} kB resident, more than \
         three times the {at_small} kB of one of {small}: each let keeps every plain let on its way"
    );
}

/// A program of `n` top-level `let`s that each call the first of a chain
/// of `n` functions, the last of which reads a `let` (rv-lt3's fnhub),
/// beside its `main.silt`.
fn program_of_a_function_hub(dir: &Path, n: usize) {
    std::fs::create_dir_all(dir).expect("a directory for the program");
    let mut main = String::from("let base = 1\n");
    for i in 0..n {
        main.push_str(&format!("fn g{i}() -> Int {{ g{}() }}\n", i + 1));
    }
    main.push_str(&format!("fn g{n}() -> Int {{ base }}\n"));
    for i in 0..n {
        main.push_str(&format!("let a{i} = g0() + {i}\n"));
    }
    main.push_str(&format!("\nfn main() {{\n  println(a{})\n}}\n", n - 1));
    std::fs::write(dir.join("main.silt"), main).expect("the main module is written");
}

/// Eight times the `let`s that each call into a chain of as many
/// functions take at most sixteen times as long to check, from 1,600 to
/// 12,800: about six and a half times (debug build, wall time as
/// measured here). Each `let`'s search walked every function it reaches:
/// n times F, 27 to 29 times as long on main 96fda6f8, and slower still
/// on 3a2cf9b5, which looked each function up in a map on that walk
/// (rv-lt3, on a quiet machine). Each node of the program is now looked
/// at once.
#[test]
fn a_function_hub_of_eight_times_the_top_level_lets_takes_at_most_sixteen_times_as_long() {
    let dir = std::env::temp_dir().join(format!(
        "silt_checker_scaling_function_hub_{}",
        std::process::id()
    ));
    let (small, large) = (1_600, 12_800);
    for n in [small, large] {
        program_of_a_function_hub(&dir.join(n.to_string()), n);
    }
    let main_of = |n: usize| dir.join(n.to_string()).join("main.silt");
    let measured = within(16.0, || best_times(&main_of(large), &main_of(small), 3));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(over) = measured {
        panic!(
            "`silt check` of {large} top-level lets that call into a chain of as many functions \
             took more than sixteen times as long as that of {small} in each of three \
             measurements ({}): each let walks every function it reaches",
            shown(&over)
        );
    }
}
