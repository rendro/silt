//! Regression lock for round-60 LATENT L9: the formatter happens to be
//! idempotent on every bundled `examples/**/*.silt` file today, and
//! each formatted example still type-checks cleanly — but no test
//! asserted either invariant across the whole corpus. A future
//! formatter change could silently regress one or both on some
//! specific example.
//!
//! This walker pins both invariants for every example:
//!
//!   1. `fmt(fmt(x)) == fmt(x)` — the formatter converges after one
//!      pass (stronger than the per-fuzz-input idempotency locks in
//!      `tests/formatter_idempotency_tests.rs`, which pin specific
//!      regressions but don't cover the example corpus).
//!
//!   2. `typecheck(fmt(x))` succeeds (no hard errors) — the formatter
//!      must never produce code that the type checker rejects, even if
//!      the source-shape change is syntactically valid.
//!
//! The companion walker `tests/examples_fmt_check_tests.rs` asserts
//! every on-disk example IS already in canonical `fmt` form (single
//! pass produces the same bytes). This file goes one step further:
//! even if someone temporarily edits an example out of canonical form,
//! the formatter must still converge on the second pass, and the
//! converged output must still type-check.

use std::path::{Path, PathBuf};

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::typechecker::{self, Severity};

/// Recursively collect every `.silt` file under `dir`. Mirrors the
/// walker in `tests/examples_check.rs` and
/// `tests/examples_fmt_check_tests.rs` so all three lock-in tests
/// cover the exact same universe of files.
fn collect_silt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            collect_silt_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("silt") {
            out.push(path);
        }
    }
}

/// Typecheck `source` and return any hard (Severity::Error) errors.
/// Warnings (e.g. `result`-shadowing) are intentionally tolerated here
/// — the companion walker `tests/examples_check.rs` already pins the
/// warning-free invariant for examples, and this test's job is only
/// to catch a formatter that produces code the type checker rejects.
fn hard_typecheck_errors(source: &str) -> Vec<String> {
    // Reset the interner so one file's interned symbols don't leak into
    // another's diagnostics. The companion walkers do the same.
    silt::intern::reset();

    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(e) => return vec![format!("lex error: {:?}", e)],
    };
    let (mut program, parse_errors) = Parser::new(tokens).parse_program_recovering();
    if !parse_errors.is_empty() {
        return parse_errors
            .iter()
            .map(|e| format!("parse error: {:?}", e))
            .collect();
    }
    typechecker::check(&mut program)
        .into_iter()
        .filter(|e| e.severity == Severity::Error)
        .map(|e| format!("type error: {:?}", e))
        .collect()
}

#[test]
fn every_example_round_trips_through_formatter_and_typechecks() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    assert!(
        examples_dir.is_dir(),
        "expected examples directory at {}",
        examples_dir.display()
    );

    let mut files = Vec::new();
    collect_silt_files(&examples_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one example .silt file under {}",
        examples_dir.display()
    );

    let mut idempotency_failures: Vec<String> = Vec::new();
    let mut typecheck_failures: Vec<String> = Vec::new();

    for file in &files {
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                idempotency_failures.push(format!("{}: failed to read: {}", file.display(), e));
                continue;
            }
        };

        // Reset the interner between files so one example's state can't
        // leak into the next's formatter pass.
        silt::intern::reset();

        let once = match silt::formatter::format(&src) {
            Ok(s) => s,
            Err(e) => {
                idempotency_failures.push(format!(
                    "{}: first format pass failed: {:?}",
                    file.display(),
                    e
                ));
                continue;
            }
        };

        silt::intern::reset();

        let twice = match silt::formatter::format(&once) {
            Ok(s) => s,
            Err(e) => {
                idempotency_failures.push(format!(
                    "{}: second format pass failed: {:?}",
                    file.display(),
                    e
                ));
                continue;
            }
        };

        if once != twice {
            // Produce a minimal hint at the first differing line so the
            // failure message points somewhere actionable without
            // dumping the whole file. Mirrors the hint style used by
            // `examples_fmt_check_tests.rs`.
            let first_diff_line = once
                .lines()
                .zip(twice.lines())
                .enumerate()
                .find(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| {
                    format!(
                        "first diff at line {}:\n  pass 1: {:?}\n  pass 2: {:?}",
                        i + 1,
                        a,
                        b
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "length differs: pass 1 {} bytes, pass 2 {} bytes",
                        once.len(),
                        twice.len()
                    )
                });
            idempotency_failures.push(format!(
                "{}: formatter is not idempotent (fmt(fmt(x)) != fmt(x)).\n{}",
                file.display(),
                first_diff_line
            ));
            // Still try to typecheck pass-1 output below — separate
            // failure bucket.
        }

        // Second invariant: the formatter's output must still typecheck.
        let errs = hard_typecheck_errors(&once);
        if !errs.is_empty() {
            typecheck_failures.push(format!(
                "{}: formatted output fails to typecheck:\n  {}",
                file.display(),
                errs.join("\n  ")
            ));
        }
    }

    assert!(
        idempotency_failures.is_empty(),
        "{} example file(s) break formatter idempotency. A future formatter \
         change that regresses round-trip stability on any bundled example \
         must be caught by this test. Fix by updating the formatter so \
         `fmt(fmt(x)) == fmt(x)` for every example.\n\n{}",
        idempotency_failures.len(),
        idempotency_failures.join("\n---\n")
    );

    assert!(
        typecheck_failures.is_empty(),
        "{} example file(s) produce formatter output that fails to \
         typecheck. The formatter must never rewrite source into a form \
         the type checker rejects — a formatter change that breaks \
         typechecking on any bundled example is a regression.\n\n{}",
        typecheck_failures.len(),
        typecheck_failures.join("\n---\n")
    );
}
