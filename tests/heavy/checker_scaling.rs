//! `silt check` is linear in the number of a module's top-level
//! definitions.
//!
//! Before stage 6 the checker generalised each function by scanning the
//! whole environment and copied the module's scope for every body, so
//! 2,000 one-line functions took four times as long as 1,000 (13 s in a
//! debug build). A function is now generalised by the levels of its own
//! type variables and its body is checked in a frame pushed on the one
//! environment; the parser finds a declaration's line in a table.

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
