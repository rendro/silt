//! Citation-lock for precise `file.rs:NNN` cross-references in code
//! comments.
//!
//! Round-96 GAP fix: ~8 of the repo's 46 precise source-line citations
//! had silently drifted to the wrong line as the cited files grew. A
//! stale citation is worse than none — it actively misleads the next
//! reader to a line that has nothing to do with the named item.
//!
//! This test pins each corrected `(citing_file, cited_file, line,
//! expected_token)` tuple: it reads the cited file's 1-based line and
//! asserts it contains the expected token. When future edits shift the
//! cited line, THIS test reds (pointing at the exact citation to
//! re-aim) instead of the citation silently re-drifting.
//!
//! To extend: when you add a new precise `foo.rs:NNN` citation in a
//! comment, add a row here so it stays honest.

use std::path::Path;

/// `(label, cited_file_relative_to_crate_root, line_1based, expected_substring)`
///
/// `label` names the citing site for a readable failure message.
const CITATIONS: &[(&str, &str, usize, &str)] = &[
    // conversions.rs:18 — "lexer increments span.col once per codepoint"
    (
        "conversions.rs -> lexer span.col increment",
        "src/lexer.rs",
        311,
        "self.col += 1",
    ),
    // completion.rs:418 — "trait method names sourced from builtin_trait_decls"
    (
        "completion.rs -> builtin_trait_decls def",
        "src/typechecker/mod.rs",
        7589,
        "fn builtin_trait_decls",
    ),
    // vm/runtime.rs:450 — "Rust 1.80+ thread-local env SAFETY note"
    (
        "vm/runtime.rs -> scheduler thread-local SAFETY",
        "src/scheduler.rs",
        1830,
        "Rust 1.80+",
    ),
    // parser.rs:3709 — "lexer rejects i64::MIN+1 literal at lex time"
    (
        "parser.rs -> lexer i64 literal too large",
        "src/lexer.rs",
        627,
        "number literal too large",
    ),
    // vm/execute.rs:1344 / vm/tests.rs:602 — And short-circuit JumpIfFalse
    (
        "execute.rs/tests.rs -> And short-circuit JumpIfFalse emit",
        "src/compiler/mod.rs",
        2327,
        "Op::JumpIfFalse",
    ),
    // vm/execute.rs:1362 / vm/tests.rs:622 — Or short-circuit JumpIfTrue
    (
        "execute.rs/tests.rs -> Or short-circuit JumpIfTrue emit",
        "src/compiler/mod.rs",
        2338,
        "Op::JumpIfTrue",
    ),
    // vm/execute.rs:1346 — `BinOp::And | BinOp::Or => unreachable!()`
    (
        "execute.rs -> And/Or unreachable guard",
        "src/compiler/mod.rs",
        2359,
        "BinOp::And | BinOp::Or => unreachable!()",
    ),
    // workspace.rs:284 — "FieldAccess.span is the receiver span" construction
    (
        "workspace.rs -> FieldAccess construction",
        "src/parser.rs",
        2598,
        "ExprKind::FieldAccess",
    ),
    // duplicate_module_not_imported_tests.rs:4 — typechecker "is not imported"
    (
        "duplicate_module test -> typechecker 'is not imported'",
        "src/typechecker/inference.rs",
        2909,
        "is not imported",
    ),
    // cli/pipeline.rs:454 — compiler "is not imported" diagnostics (3 sites)
    (
        "pipeline.rs -> compiler 'is not imported' #1",
        "src/compiler/mod.rs",
        2562,
        "is not imported",
    ),
    (
        "pipeline.rs -> compiler 'is not imported' #2",
        "src/compiler/mod.rs",
        2658,
        "is not imported",
    ),
    (
        "pipeline.rs -> compiler 'is not imported' #3",
        "src/compiler/mod.rs",
        3477,
        "is not imported",
    ),
    // compiler/mod.rs:441 — typechecker round-58 prefix-mirror logic
    (
        "compiler/mod.rs -> typechecker round-58 prefix-mirror",
        "src/typechecker/mod.rs",
        2941,
        "round 58",
    ),
];

#[test]
fn precise_source_line_citations_resolve() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();

    for &(label, rel, line_1based, expected) in CITATIONS {
        let path = crate_root.join(rel);
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("[{label}] cannot read {rel}: {e}"));
                continue;
            }
        };
        let line = src.lines().nth(line_1based - 1);
        match line {
            Some(text) if text.contains(expected) => {}
            Some(text) => failures.push(format!(
                "[{label}] {rel}:{line_1based} no longer contains {expected:?}\n     actual: {}",
                text.trim()
            )),
            None => failures.push(format!(
                "[{label}] {rel}:{line_1based} is past end of file ({} lines)",
                src.lines().count()
            )),
        }
    }

    assert!(
        failures.is_empty(),
        "{} stale source-line citation(s) — re-aim the comment(s) and update this lock:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
