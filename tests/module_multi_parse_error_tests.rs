//! Round-52 deferred item 5: imported modules must surface ALL parse
//! errors, not just the first one.
//!
//! Before this fix, `src/compiler/mod.rs::compile_file_module_inner`
//! parsed imported modules with the non-recovering `parse_program()`
//! path, so a module with three independent header-level mistakes would
//! bail at the first, leaving the user to fix-then-rerun twice more
//! before seeing the rest. The entrypoint file already used
//! `parse_program_recovering` and surfaced every diagnostic in one go.
//!
//! The fix switches the imported-module path to the same recovering
//! parser and propagates the extra errors to the CLI via
//! `Compiler::module_parse_errors()`, which the pipeline drains
//! alongside the primary `Err` returned from `compile_program`.
//!
//! These tests pin:
//!
//!   1. A module with three distinct parse errors at three distinct
//!      lines yields at least three compile-phase diagnostics, each
//!      naming the imported-module file path and pointing at a column
//!      > 0 (so the single-line/caret rendering still works for every
//!      accumulated error, not just the first).
//!
//!   2. Negative control — a module with zero parse errors compiles
//!      cleanly with zero compile-phase diagnostics, locking that the
//!      new Vec path doesn't leak false positives into the happy path.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use silt::compiler::Compiler;
use silt::errors::SourceError;
use silt::intern;
use silt::lexer::Lexer;
use silt::parser::Parser;

/// Mirror of the `compiler_for_root` helper in `tests/modules.rs`:
/// wraps a temp directory as the synthetic `__local__` package so the
/// compiler can resolve `import foo` to `<root>/foo.silt` without
/// needing a real `silt.toml`/`src/` layout.
fn compiler_for_root(root: PathBuf) -> Compiler {
    let local = intern::intern("__local__");
    let mut roots = HashMap::new();
    roots.insert(local, root);
    Compiler::with_package_roots(local, roots)
}

/// Fresh per-test temp directory under `/tmp/` so parallel test runs
/// don't collide on fixture filenames.
fn tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir()
        .join(format!("silt_multierr_{}_{}", std::process::id(), nanos));
    fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

/// Three broken `pub fn` headers at three distinct lines. Each header
/// opens a parameter list with `(` and then immediately hits `{`,
/// which the recovery parser diagnoses as "expected parameter name,
/// found {". The errors land at lines 1, 4, 7 (column 14 or 15), which
/// is a distinct span per error.
const BROKEN_MODULE: &str = "\
pub fn alpha( {
  1
}
pub fn beta( {
  2
}
pub fn gamma( {
  3
}
";

/// Main entrypoint that imports the broken module. The compile path
/// inside `compile_program` will recurse into `compile_file_module`,
/// which is where the round-52 change lives.
const MAIN_USING_BROKEN_MODULE: &str = "\
import broken

fn main() {
  broken.alpha()
}
";

/// Locking test: the compiler MUST surface every parse error that the
/// recovery parser saw in the imported module, not just the first.
/// The primary is returned as `Err`; the rest are accumulated on
/// `Compiler::module_parse_errors()`.
#[test]
fn test_imported_module_reports_all_parse_errors() {
    let dir = tempdir();
    fs::write(dir.join("broken.silt"), BROKEN_MODULE).expect("write broken.silt");

    let tokens = Lexer::new(MAIN_USING_BROKEN_MODULE)
        .tokenize()
        .expect("main lex");
    let mut program = Parser::new(tokens).parse_program().expect("main parse");
    let _ = silt::typechecker::check(&mut program);

    let mut compiler = compiler_for_root(dir.clone());
    let primary = match compiler.compile_program(&program) {
        Ok(_) => panic!("expected compile error from broken module"),
        Err(e) => e,
    };

    // Assemble the full diagnostic batch the way the CLI pipeline does:
    // primary first, then the accumulated extras in source order.
    let mut all_errors = vec![primary.clone()];
    all_errors.extend(compiler.module_parse_errors().iter().cloned());

    // There were three distinct broken headers in the fixture; we
    // require AT LEAST three accumulated diagnostics. Using `>=` (not
    // `==`) lets a future parser improvement that reports even more
    // fine-grained errors pass without churn.
    assert!(
        all_errors.len() >= 3,
        "expected at least 3 parse diagnostics from broken module, got {}: {:?}",
        all_errors.len(),
        all_errors
            .iter()
            .map(|e| e.message.lines().next().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    );

    // Every accumulated error must carry the imported-module's file
    // path in its pre-formatted message (the outer span points at the
    // `import broken` statement in the entrypoint, but the embedded
    // snippet is the module's own source). This pins the
    // `format_module_source_error`-based formatting: dropping it for
    // extras would silently lose the "which file" context.
    for (i, err) in all_errors.iter().enumerate() {
        assert!(
            err.message.contains("broken.silt"),
            "diagnostic #{i} must name the broken module file, got message:\n{}",
            err.message
        );
        assert!(
            err.message.contains("parse error"),
            "diagnostic #{i} must describe the error kind as parse error, got:\n{}",
            err.message
        );
    }

    // Spans for the extras MUST lift through SourceError with caret
    // columns > 0 — the audit trap was that losing span fidelity would
    // leave every extra pointing at col 0 / line 0, which renders with
    // no caret and breaks the diagnostic UX. Use the same lifting the
    // pipeline uses so this is a full end-to-end check.
    let source_errors: Vec<SourceError> = all_errors
        .iter()
        .map(|e| {
            SourceError::from_compile_error(e, MAIN_USING_BROKEN_MODULE, "main.silt")
        })
        .collect();

    // At least three diagnostics must have meaningful spans (col > 0).
    // All extras share the same outer `import` span today (that's the
    // "outer caret" that points at the import site, which is fine);
    // the inner snippet embedded in the message body is what carries
    // per-error column info. We pin both: (a) the outer caret isn't
    // degenerate, AND (b) each rendered body contains a non-zero
    // "line:col" locator for the inner position.
    for (i, se) in source_errors.iter().enumerate() {
        assert!(
            se.span.col > 0,
            "diagnostic #{i} outer span col must be > 0, got col={} line={}",
            se.span.col,
            se.span.line
        );
    }

    // Distinct inner-line locators — each broken header is on a
    // different module-source line, so the pre-formatted messages
    // must embed three different `broken.silt:LINE:COL` tags. This
    // is the strongest anti-regression guard: a bug that copied the
    // primary's formatted message into every extra slot would collapse
    // all three into the same string.
    let mut distinct_inner_locators = std::collections::HashSet::new();
    for err in &all_errors {
        // Extract substrings matching "broken.silt:N:M" — cheap scan.
        for (idx, _) in err.message.match_indices("broken.silt:") {
            let tail = &err.message[idx..];
            // Grab through the first whitespace or newline.
            let end = tail
                .find(|c: char| c == ' ' || c == '\n' || c == '\t')
                .unwrap_or(tail.len());
            distinct_inner_locators.insert(tail[..end].to_string());
            break; // one inner locator per error is enough
        }
    }
    assert!(
        distinct_inner_locators.len() >= 3,
        "expected at least 3 distinct inner locators (line:col) across module \
         parse errors, got {:?}",
        distinct_inner_locators
    );
}

/// Negative control: a module with zero parse errors still compiles
/// cleanly. Pins that the new `module_parse_errors` drain doesn't
/// manufacture false diagnostics — the happy path must stay silent.
#[test]
fn test_clean_imported_module_emits_no_extra_errors() {
    let dir = tempdir();
    let clean_module = "\
pub fn add(a, b) = a + b
pub fn mul(a, b) = a * b
";
    fs::write(dir.join("clean.silt"), clean_module).expect("write clean.silt");

    let main_src = "\
import clean

fn main() {
  clean.add(1, 2)
}
";
    let tokens = Lexer::new(main_src).tokenize().expect("main lex");
    let mut program = Parser::new(tokens).parse_program().expect("main parse");
    let _ = silt::typechecker::check(&mut program);

    let mut compiler = compiler_for_root(dir.clone());
    let result = compiler.compile_program(&program);

    assert!(
        result.is_ok(),
        "clean module must compile without error, got: {:?}",
        result.err().map(|e| e.message)
    );
    assert_eq!(
        compiler.module_parse_errors().len(),
        0,
        "clean module must not populate module_parse_errors; got {} entries",
        compiler.module_parse_errors().len()
    );
}
