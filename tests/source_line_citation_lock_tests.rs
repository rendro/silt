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
        7827,
        "fn builtin_trait_decls",
    ),
    // vm/runtime.rs:450 — "Rust 1.80+ thread-local env SAFETY note"
    (
        "vm/runtime.rs -> scheduler thread-local SAFETY",
        "src/scheduler.rs",
        1830,
        "Rust 1.80+",
    ),
    // parser.rs:3727 — "lexer rejects i64::MIN+1 literal at lex time"
    (
        "parser.rs -> lexer i64 literal too large",
        "src/lexer.rs",
        632,
        "number literal too large",
    ),
    // vm/execute.rs:1420 / vm/tests.rs:602 — And short-circuit JumpIfFalse
    (
        "execute.rs/tests.rs -> And short-circuit JumpIfFalse emit",
        "src/compiler/mod.rs",
        2330,
        "Op::JumpIfFalse",
    ),
    // vm/execute.rs:1438 / vm/tests.rs:622 — Or short-circuit JumpIfTrue
    (
        "execute.rs/tests.rs -> Or short-circuit JumpIfTrue emit",
        "src/compiler/mod.rs",
        2341,
        "Op::JumpIfTrue",
    ),
    // vm/execute.rs:1422 — `BinOp::And | BinOp::Or => unreachable!()`
    (
        "execute.rs -> And/Or unreachable guard",
        "src/compiler/mod.rs",
        2362,
        "BinOp::And | BinOp::Or => unreachable!()",
    ),
    // workspace.rs:444 — "FieldAccess.span is the receiver span" construction
    (
        "workspace.rs -> FieldAccess construction",
        "src/parser.rs",
        2620,
        "ExprKind::FieldAccess",
    ),
    // duplicate_module_not_imported_tests.rs:4 — typechecker "is not imported"
    (
        "duplicate_module test -> typechecker 'is not imported'",
        "src/typechecker/inference.rs",
        2903,
        "is not imported",
    ),
    // cli/pipeline.rs:454 — compiler "is not imported" diagnostics (3 sites)
    (
        "pipeline.rs -> compiler 'is not imported' #1",
        "src/compiler/mod.rs",
        2565,
        "is not imported",
    ),
    (
        "pipeline.rs -> compiler 'is not imported' #2",
        "src/compiler/mod.rs",
        2661,
        "is not imported",
    ),
    (
        "pipeline.rs -> compiler 'is not imported' #3",
        "src/compiler/mod.rs",
        3480,
        "is not imported",
    ),
    // compiler/mod.rs:441 — typechecker round-58 prefix-mirror logic
    (
        "compiler/mod.rs -> typechecker round-58 prefix-mirror",
        "src/typechecker/mod.rs",
        3043,
        "round 58",
    ),
    // Round-101 re-aimed cross-file citations (bare/short-path cites that
    // both citation locks previously missed). Each row pins the re-aimed
    // TARGET line; `reaimed_cross_file_citations_do_not_regress` below
    // pins the CITING text so neither side can drift alone.
    // formatter.rs — interpolation continuation span capture
    (
        "formatter.rs -> lexer interp cont_start capture",
        "src/lexer.rs",
        829,
        "let cont_start = self.span()",
    ),
    // typechecker/mod.rs — Float/ExtFloat widening block header
    (
        "typechecker/mod.rs -> inference.rs Float/ExtFloat widening",
        "src/typechecker/inference.rs",
        3435,
        "Implicit Float → ExtFloat widening",
    ),
    // typechecker/mod.rs — FieldAccess arm's Unit dispatch key
    (
        "typechecker/mod.rs -> inference.rs FieldAccess Unit key",
        "src/typechecker/inference.rs",
        3215,
        r#"Type::Unit => intern("Unit")"#,
    ),
    // Round-103 batch: further unlocked citations that had re-drifted
    // (never in this table). Each is re-aimed in its comment and pinned
    // here.
    // inference.rs binop arms — cascade-suppression branch in `unify`
    (
        "inference.rs Add/Sub/Div arms -> unify cascade-suppression branch",
        "src/typechecker/mod.rs",
        1387,
        "(Type::Error, _) | (_, Type::Error)",
    ),
    // inference.rs — `generalize` EffectSet::TOP caller-MUST warning
    (
        "inference.rs alias-effects helper -> generalize TOP warning",
        "src/typechecker/mod.rs",
        1835,
        "Every caller MUST",
    ),
    // typechecker/mod.rs — unify (Range, List) cross-arm
    (
        "typechecker/mod.rs canonicalise -> unify Range/List cross-arm",
        "src/typechecker/mod.rs",
        1492,
        "(Type::Range(a), Type::List(b))",
    ),
    // typechecker/mod.rs — register_fn_decl where-clause-tyvar error
    (
        "typechecker/mod.rs impl where-clause -> register_fn_decl error",
        "src/typechecker/mod.rs",
        5163,
        "in where clause is not introduced",
    ),
    // typechecker/mod.rs — auto-derive registration under "Unit"
    (
        "typechecker/mod.rs () -> Unit collapse -> auto-derive Unit key",
        "src/typechecker/mod.rs",
        8060,
        "[\"Int\", \"Float\", \"ExtFloat\", \"Bool\", \"String\", \"Unit\"]",
    ),
    // typechecker/mod.rs — pass-3 remap loop at trait-impl recheck
    (
        "typechecker/mod.rs align_tyvars_into -> trait-impl recheck remap",
        "src/typechecker/mod.rs",
        3583,
        "remap.get(old_tv).map(|&new_tv|",
    ),
    // typechecker/mod.rs — Float/Float -> ExtFloat widening (Div arm)
    (
        "typechecker/mod.rs auto-derive ExtFloat -> Div widening arm",
        "src/typechecker/inference.rs",
        3593,
        "(Type::Float, Type::Float)",
    ),
    // inference.rs — parser G1 foreign-keyword hint table
    (
        "inference.rs undefined-variable hints -> parser G1 hint table",
        "src/parser.rs",
        2245,
        "fn foreign_keyword_hint",
    ),
    // types/mod.rs — parser fn-type annotation construction
    (
        "types/mod.rs Display tests -> parser fn-type annotation parse",
        "src/parser.rs",
        2116,
        "TypeExprKind::Function(params, Box::new(ret))",
    ),
    // typechecker/mod.rs — Bytes runtime Display helper
    (
        "typechecker/mod.rs -> value.rs format_bytes_preview",
        "src/value.rs",
        1364,
        "fn format_bytes_preview",
    ),
    // vm/dispatch.rs — Value::cmp List/Range pairings
    (
        "vm/dispatch.rs Compare arm -> arithmetic List/Range cmp arm",
        "src/vm/arithmetic.rs",
        152,
        "(Value::List(_), Value::List(_))",
    ),
];

/// `(citing_file_relative_to_crate_root, stale_citation_substring)`
///
/// Round-103: the 12 drifted citations fixed in that round must not
/// reappear verbatim — each pointed at a line that has nothing to do
/// with the named item. This is the "fails before the fix" half of the
/// lock; the CITATIONS rows above are the "stays fixed" half.
const STALE_CITATIONS: &[(&str, &str)] = &[
    ("src/typechecker/inference.rs", "mod.rs:741`"),
    ("src/typechecker/inference.rs", "mod.rs:1593-1601"),
    ("src/typechecker/inference.rs", "parser.rs:2084-2118"),
    ("src/typechecker/mod.rs", "mod.rs:594`"),
    ("src/typechecker/mod.rs", "mod.rs:1690"),
    ("src/typechecker/mod.rs", "inference.rs:2582"),
    ("src/typechecker/mod.rs", "mod.rs:6983"),
    ("src/typechecker/mod.rs", "mod.rs:3217"),
    ("src/typechecker/mod.rs", "inference.rs:2326-2333"),
    ("src/typechecker/mod.rs", "value.rs:1258"),
    ("src/types/mod.rs", "parser.rs:838"),
    ("src/formatter.rs", "lexer.rs:748"),
    ("src/vm/dispatch.rs", "arithmetic.rs:138"),
];

#[test]
fn stale_round103_citations_do_not_reappear() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();

    for &(rel, stale) in STALE_CITATIONS {
        let path = crate_root.join(rel);
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"));
        if src.contains(stale) {
            failures.push(format!(
                "{rel} still contains stale citation {stale:?} — re-aim it \
                 (see the round-103 rows in CITATIONS for the current targets)"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} stale citation(s) resurfaced:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

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

/// Round-101 GAP fix: 8 precise citations had silently drifted — they
/// were spelled with bare / short paths (`lexer.rs:748`, `mod.rs:594`,
/// `inference.rs:2582`, `src/value.rs:1258`, ...) so neither this file's
/// fixed list nor `typechecker_citation_resolve_tests.rs`'s full-path
/// needle covered them.
///
/// This test pins the CITING side of the re-aimed cross-file citations:
/// each citing file must contain the corrected `file.rs:<N>` text and
/// must NOT contain the old drifted number. Together with the CITATIONS
/// rows above (which pin the cited TARGET lines), a future edit that
/// shifts a target line reds the resolve row, and a comment edit that
/// re-drifts the number reds this grep-lock.
#[test]
fn reaimed_cross_file_citations_do_not_regress() {
    let formatter = include_str!("../src/formatter.rs");
    let tc_mod = include_str!("../src/typechecker/mod.rs");
    let tc_inference = include_str!("../src/typechecker/inference.rs");

    // (citing_source, label, must_contain, must_not_contain)
    let checks: &[(&str, &str, &str, &str)] = &[
        (
            formatter,
            "src/formatter.rs interp block-comment note",
            "lexer.rs:829 `cont_start = self.span()`",
            "lexer.rs:748",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs Phase-B Range canonicalisation note",
            "`mod.rs:1492`",
            "mod.rs:594",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs impl-where-clause error note",
            "register_fn_decl error at mod.rs:5163",
            "mod.rs:1690",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs `() -> Unit` collapse note",
            "(inference.rs:3215) and auto-derive (`mod.rs:8060`)",
            "inference.rs:2582",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs Round-75 TYPE-2 remap-loop note",
            "mod.rs:3583) would then drop those constraints",
            "mod.rs:3217",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs ExtFloat auto-derive note",
            "`src/typechecker/inference.rs:3435-3451`",
            "inference.rs:2326",
        ),
        (
            tc_mod,
            "src/typechecker/mod.rs Bytes Display note",
            "`format_bytes_preview` at src/value.rs:1364",
            "src/value.rs:1258",
        ),
        (
            tc_inference,
            "src/typechecker/inference.rs alias effect-widening doc",
            "`generalize` at `mod.rs:1834-1842`",
            "mod.rs:1593",
        ),
        (
            tc_inference,
            "src/typechecker/inference.rs cascade-suppression cites",
            "(`mod.rs:1387`)",
            "mod.rs:741",
        ),
    ];

    let mut failures = Vec::new();
    for &(src, label, good, bad) in checks {
        if !src.contains(good) {
            failures.push(format!(
                "[{label}] no longer contains the re-aimed citation {good:?} — \
                 if the comment was re-aimed again, update this lock in the same patch"
            ));
        }
        if src.contains(bad) {
            failures.push(format!(
                "[{label}] the stale citation {bad:?} reappeared — it points at \
                 unrelated code; re-aim it (see CITATIONS rows for the real target)"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} regressed cross-file citation(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}
