//! The check of a module is linear in the number of its top-level
//! definitions.
//!
//! Before stage 6 the checker generalised each function by scanning the
//! whole environment and copied the module's scope for every body, so
//! 2,000 one-line functions took four times as long as 1,000. A function
//! is now generalised by the levels of its own type variables and its
//! body is checked in a frame pushed on the one environment.
//!
//! The test times the resolver and the checker on a module the parser
//! has already read: what is measured is the analysis. It reads the
//! thread's own clock, so that other work on the machine does not count,
//! and runs where there is one (Unix).

#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use silt::ast::Program;
use silt::diagnostic::Diagnostic;
use silt::intern::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::session::ModuleId;
use silt::source::FileId;
use silt::typechecker::{self, ModuleContext, Tables, names};

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

/// How long this thread has run: what a check costs, however busy the
/// machine is with other work.
fn thread_time() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid timespec for the call to fill.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
    assert_eq!(status, 0, "the thread's clock is readable");
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// The module `source` as the parser reads it. The source must be free
/// of errors.
fn parse(source: &str) -> Program {
    let tokens = Lexer::new(FileId::default(), source)
        .tokenize()
        .expect("the module lexes");
    let (program, errors) = Parser::new(tokens, source).parse_program_recovering();
    assert!(errors.is_empty(), "the module parses: {errors:?}");
    program
}

/// How long the analysis of `program` takes: resolving its names and
/// checking its types. The module must be free of errors.
fn analysis_time(program: &Program) -> Duration {
    let mut program = program.clone();
    let module = ModuleId(0);
    let mut defs = names::new_def_table();
    let mut tables = Tables::for_session();
    let started = thread_time();
    let resolution = names::resolve_module(
        &mut program,
        module,
        names::ModuleKind::File,
        &HashMap::new(),
        &mut defs,
    );
    let check = typechecker::check_module(
        &mut program,
        ModuleContext {
            module,
            module_name: intern("main"),
            kind: names::ModuleKind::File,
            package: None,
            scope: &resolution.scope,
            earlier: &[],
            defs: Arc::new(defs),
            tables: &mut tables,
        },
    );
    let elapsed = thread_time() - started;
    let problems: Vec<&Diagnostic> = resolution
        .diagnostics
        .iter()
        .chain(&check.diagnostics)
        .collect();
    assert!(problems.is_empty(), "the module checks: {problems:?}");
    elapsed
}

#[test]
fn checking_4000_functions_takes_about_twice_as_long_as_2000() {
    let small = parse(&module_of(2_000));
    let large = parse(&module_of(4_000));
    // The first check of a thread builds the builtin environment.
    analysis_time(&parse(&module_of(10)));
    // The best of many runs of each, in turn: a run is short.
    let mut best_small = Duration::MAX;
    let mut best_large = Duration::MAX;
    for _ in 0..20 {
        best_small = best_small.min(analysis_time(&small));
        best_large = best_large.min(analysis_time(&large));
    }
    let ratio = best_large.as_secs_f64() / best_small.as_secs_f64();
    assert!(
        ratio <= 2.3,
        "checking 4,000 functions took {best_large:?}, {ratio:.2} times the {best_small:?} of \
         2,000 functions: the check is no longer linear in the number of definitions"
    );
}
