//! Round-101 GAP lock: PatternKind walker parity across every
//! binder-collecting pattern walker.
//!
//! The authoritative binder collector is
//! `src/typechecker/mod.rs::collect_pattern_vars`. Seven other walkers
//! re-enumerate `PatternKind` to find pattern binders:
//!
//!   * `src/lsp/locals.rs::collect_pattern_names`        (completion locals)
//!   * `src/lsp/local_bindings.rs::collect_pattern_bindings` (hover/rename local gate)
//!   * `src/lsp/definitions.rs::collect_let_pattern_defs` (goto-def for top-level lets)
//!   * `src/lsp/workspace.rs::collect_references_in_pattern` (rename/references)
//!   * `src/lsp/ast_walk.rs::find_ident_in_pattern`       (cursor→symbol resolution)
//!   * `src/lsp/semantic_tokens.rs::emit_pattern_binding_tokens` (highlighting)
//!   * `src/repl.rs::collect_pattern_names`               (REPL tab completion)
//!
//! Pre-round-101 these drifted from `collect_pattern_vars`: the
//! AnonRecord `...rest` binder, `Map` value binders, `Or` alternative
//! binders, and (in two walkers) the `List` rest sub-pattern were all
//! invisible to LSP/REPL tooling even though they are shipped runtime
//! features (see tests/anon_record_rest_pattern_runtime_tests.rs). The
//! ExprKind walker got a parity lock in round 85
//! (tests/round85_visit_expr_children_parity_lock_tests.rs); the
//! PatternKind walkers never did — which is exactly why they drifted.
//!
//! Strategy (mirrors the round-85 lock):
//!   * Parse every variant head from `pub enum PatternKind { ... }` in
//!     src/ast.rs.
//!   * Subtract the known NON-binding leaves (literals, wildcard,
//!     ranges, pin) — everything left can introduce a binder.
//!   * For each walker fn body, assert every binding-capable variant is
//!     named (`PatternKind::<V>`).
//!   * Additionally assert no walker destructures
//!     `AnonRecord { fields, .. }` (dropping the rest binder symbol) or
//!     `List(pats, _)` (dropping the rest sub-pattern).
//!
//! A behavioral REPL lock rides along: `let {x, ...rest} = r` must
//! surface BOTH `x` and `rest` to tab completion
//! (`repl::collect_decl_completion_names` is the exact function the
//! REPL calls after each declaration). The LSP-side behavioral locks
//! live in the walkers' own unit-test modules
//! (src/lsp/locals.rs, src/lsp/semantic_tokens.rs).

use std::collections::BTreeSet;

use silt::lexer::Lexer;
use silt::parser::Parser;
use silt::repl::collect_decl_completion_names;

const AST_SRC: &str = include_str!("../src/ast.rs");

/// (file label, source text, binder-walker fn signature head)
const WALKERS: &[(&str, &str, &str)] = &[
    (
        "src/lsp/locals.rs",
        include_str!("../src/lsp/locals.rs"),
        "fn collect_pattern_names(",
    ),
    (
        "src/lsp/local_bindings.rs",
        include_str!("../src/lsp/local_bindings.rs"),
        "fn collect_pattern_bindings(",
    ),
    (
        "src/lsp/definitions.rs",
        include_str!("../src/lsp/definitions.rs"),
        "fn collect_let_pattern_defs(",
    ),
    (
        "src/lsp/workspace.rs",
        include_str!("../src/lsp/workspace.rs"),
        "fn collect_references_in_pattern(",
    ),
    (
        "src/lsp/ast_walk.rs",
        include_str!("../src/lsp/ast_walk.rs"),
        "fn find_ident_in_pattern(",
    ),
    (
        "src/lsp/semantic_tokens.rs",
        include_str!("../src/lsp/semantic_tokens.rs"),
        "fn emit_pattern_binding_tokens(",
    ),
    (
        "src/repl.rs",
        include_str!("../src/repl.rs"),
        "fn collect_pattern_names(",
    ),
];

/// PatternKind variants that can never introduce a binder, per the
/// authoritative `collect_pattern_vars` in src/typechecker/mod.rs
/// (its final grouped arm returns `vec![]` for exactly these). `Pin`
/// REFERENCES an existing variable rather than binding a new one. If a
/// variant is removed from this list (i.e. it grows binders), every
/// walker above must gain an arm for it.
fn non_binding_leaf_variants() -> BTreeSet<String> {
    [
        "Wildcard",
        "Int",
        "Float",
        "Bool",
        "StringLit",
        "Range",
        "FloatRange",
        "Pin",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Parse every top-level variant head from `pub enum PatternKind { ... }`
/// in src/ast.rs (same line-oriented, depth-tracked scan as the round-85
/// ExprKind lock).
fn patternkind_variants() -> BTreeSet<String> {
    let enum_start = AST_SRC
        .find("pub enum PatternKind {")
        .expect("ast.rs no longer contains `pub enum PatternKind {` — has the enum moved?");
    let bytes = AST_SRC.as_bytes();
    let mut depth = 0i32;
    let mut i = enum_start + "pub enum PatternKind {".len() - 1; // start at the `{`
    let mut enum_end = enum_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    enum_end = i + 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    assert!(
        enum_end > enum_start,
        "could not locate end of `pub enum PatternKind` block in ast.rs"
    );
    let body = &AST_SRC[enum_start..enum_end];

    let mut variants_seen = BTreeSet::new();
    let mut d = 0i32;
    let mut current_line_start_depth = 0i32;
    let mut line_buf = String::new();
    for c in body.chars() {
        if c == '\n' {
            if current_line_start_depth == 1 {
                let trimmed = line_buf.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with("/*")
                    && !trimmed.starts_with("*")
                {
                    let first_token: String = trimmed
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !first_token.is_empty()
                        && first_token
                            .chars()
                            .next()
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                    {
                        let rest = trimmed[first_token.len()..].trim_start();
                        let starts_ok = rest.starts_with('(')
                            || rest.starts_with('{')
                            || rest.starts_with(',')
                            || rest.is_empty();
                        if starts_ok {
                            variants_seen.insert(first_token);
                        }
                    }
                }
            }
            line_buf.clear();
            current_line_start_depth = d;
            continue;
        }
        if c == '{' {
            d += 1;
        } else if c == '}' {
            d -= 1;
        }
        line_buf.push(c);
    }
    variants_seen
}

/// Extract the body of the fn whose signature starts with `fn_sig` from
/// `src`. Line comments are stripped BEFORE brace matching because
/// walker comments legitimately contain unbalanced braces (e.g. "the
/// opening `{` for an anon record"). None of the target fn bodies
/// contain string/char literals with braces, so comment-stripped brace
/// matching is exact for them.
fn fn_body(file: &str, src: &str, fn_sig: &str) -> String {
    let idx = src
        .find(fn_sig)
        .unwrap_or_else(|| panic!("{file} no longer contains `{fn_sig}` — has the walker moved or been renamed? Update tests/round101_pattern_binder_walker_parity_lock_tests.rs"));
    let after = &src[idx..];
    let mut stripped = String::new();
    for line in after.lines() {
        let code = match line.find("//") {
            Some(p) => &line[..p],
            None => line,
        };
        stripped.push_str(code);
        stripped.push('\n');
    }
    let open = stripped
        .find('{')
        .unwrap_or_else(|| panic!("{file}: `{fn_sig}` has no body"));
    let bytes = stripped.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return stripped[open..=i].to_string();
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("{file}: unbalanced braces while extracting `{fn_sig}` body");
}

#[test]
fn every_binding_capable_patternkind_variant_is_named_in_every_walker() {
    let variants = patternkind_variants();
    let leaves = non_binding_leaf_variants();
    assert!(
        !variants.is_empty(),
        "failed to parse any PatternKind variants from ast.rs — parser changed shape"
    );

    let mut failures: Vec<String> = Vec::new();
    for (file, src, fn_sig) in WALKERS {
        let body = fn_body(file, src, fn_sig);
        for v in &variants {
            if leaves.contains(v) {
                continue;
            }
            if !body.contains(&format!("PatternKind::{v}")) {
                failures.push(format!("{file} `{fn_sig}` is missing `PatternKind::{v}`"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "binder-collecting pattern walker(s) drifted from the authoritative \
         `collect_pattern_vars` (src/typechecker/mod.rs):\n  {}\n\
         Every binding-capable PatternKind variant must have an arm in each \
         walker — an omission makes the binder invisible to LSP \
         completion/rename/goto-def/semantic-tokens or REPL tab completion. \
         If the variant genuinely cannot bind, add it to \
         `non_binding_leaf_variants()` in this test instead.",
        failures.join("\n  ")
    );
}

#[test]
fn no_walker_drops_the_anon_record_rest_binder() {
    // `PatternKind::AnonRecord { fields, .. }` silently discards the
    // `rest: Option<Symbol>` binder (`{x, ...rest}` binds `rest`). Every
    // walker must destructure `rest` by name.
    let mut failures: Vec<String> = Vec::new();
    for (file, src, fn_sig) in WALKERS {
        let body = fn_body(file, src, fn_sig);
        let mut search: &str = &body;
        while let Some(pos) = search.find("PatternKind::AnonRecord {") {
            let after = &search[pos + "PatternKind::AnonRecord {".len()..];
            let head_end = after.find('}').unwrap_or(after.len());
            let head = &after[..head_end];
            if !head.contains("rest") {
                failures.push(format!(
                    "{file} `{fn_sig}` destructures AnonRecord as `{{{}}}` — the \
                     `rest` binder is dropped",
                    head.trim()
                ));
            }
            search = after;
        }
    }
    assert!(
        failures.is_empty(),
        "AnonRecord rest binder dropped:\n  {}\n\
         Destructure `PatternKind::AnonRecord {{ fields, rest }}` and bind \
         `rest` like the authoritative `collect_pattern_vars` does.",
        failures.join("\n  ")
    );
}

#[test]
fn no_walker_drops_the_list_rest_subpattern() {
    // `PatternKind::List(pats, _)` silently discards the rest
    // sub-pattern (`[h, ..t]` binds `t`). Every walker must name and
    // recurse into it.
    let mut failures: Vec<String> = Vec::new();
    for (file, src, fn_sig) in WALKERS {
        let body = fn_body(file, src, fn_sig);
        let mut search: &str = &body;
        while let Some(pos) = search.find("PatternKind::List(") {
            let after = &search[pos + "PatternKind::List(".len()..];
            let head_end = after.find(')').unwrap_or(after.len());
            let head = &after[..head_end];
            if head.contains(", _") {
                failures.push(format!(
                    "{file} `{fn_sig}` destructures List as `({head})` — the rest \
                     sub-pattern is dropped"
                ));
            }
            search = after;
        }
    }
    assert!(
        failures.is_empty(),
        "List rest sub-pattern dropped:\n  {}\n\
         Destructure `PatternKind::List(pats, rest)` and recurse into \
         `rest` like the authoritative `collect_pattern_vars` does.",
        failures.join("\n  ")
    );
}

#[test]
fn non_binding_leaf_list_only_contains_real_variants() {
    let variants = patternkind_variants();
    for leaf in &non_binding_leaf_variants() {
        assert!(
            variants.contains(leaf),
            "`non_binding_leaf_variants()` lists `{leaf}` but no such variant \
             exists in `pub enum PatternKind` in src/ast.rs. Fix the typo or \
             remove the stale entry. Live variants: {variants:?}"
        );
    }
}

// ── Behavioral lock: REPL tab completion ──────────────────────────

#[test]
fn repl_completion_names_include_anon_record_rest_binder() {
    // Round-101 repro 4: after `let {x, ...rest} = r`, REPL tab
    // completion offered `x` but not `rest`. This drives the exact
    // helper `eval_declaration` uses to populate its completion list.
    let tokens = Lexer::new("let {x, ...rest} = r")
        .tokenize()
        .unwrap_or_else(|e| panic!("fixture failed to lex: {}", e.message));
    let program = Parser::new(tokens)
        .parse_program()
        .unwrap_or_else(|e| panic!("fixture failed to parse: {}", e.message));
    let names = collect_decl_completion_names(&program.decls);
    assert_eq!(
        names,
        vec!["x".to_string(), "rest".to_string()],
        "`let {{x, ...rest}} = r` must surface BOTH the shorthand field \
         and the rest binder to REPL completion"
    );
}
