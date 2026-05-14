//! Round 85 parity lock: `visit_expr_children` in `src/lsp/ast_walk.rs`
//! enumerates child-bearing `ExprKind` variants. The LSP walker is the
//! foundation for find-references, hover, inlay-hints, selection-range,
//! and folding. Adding a new `ExprKind` variant without updating the
//! walker silently degrades every one of those features.
//!
//! Round-85 G3 follow-up: the prior `_ => {}` catch-all has been
//! removed from `visit_expr_children`. **Compiler exhaustiveness is now
//! the primary gate** — adding a new `ExprKind` variant without a
//! walker arm fails the `cargo build` step. The source-grep parity
//! lock in this file is now belt-and-suspenders: it catches the
//! specific regression where someone re-introduces a `_ => {}`
//! wildcard (which would silently re-open the gap), and it cross-
//! checks the `ExprKind` ↔ walker mapping at the source-text level.
//!
//! Strategy (retained for belt-and-suspenders coverage):
//!   * Read `src/ast.rs` via `include_str!`; parse every top-level
//!     `ExprKind::Variant(...)` / `ExprKind::Variant { ... }` /
//!     `Variant,` head from inside the `pub enum ExprKind { ... }` block.
//!   * Read `src/lsp/ast_walk.rs` via `include_str!`; locate the
//!     `pub(super) fn visit_expr_children(` body and parse every
//!     `ExprKind::Variant` mentioned in it.
//!   * Assert that every variant from `ExprKind` is either (a) named in
//!     the walker body OR (b) explicitly allowed as a "leaf" with no
//!     child expressions to walk (literals, idents, unit, etc.).
//!   * Additionally, assert that the walker body does NOT contain a
//!     `_ => {}` catch-all (round-85 G3 lock).

use std::collections::BTreeSet;

const AST_SRC: &str = include_str!("../src/ast.rs");
const WALK_SRC: &str = include_str!("../src/lsp/ast_walk.rs");

/// Parse every top-level variant head from `pub enum ExprKind { ... }`.
fn exprkind_variants() -> BTreeSet<String> {
    let enum_start = AST_SRC
        .find("pub enum ExprKind {")
        .expect("ast.rs no longer contains `pub enum ExprKind {` — has the enum moved?");
    // Find the matching close brace at depth 0.
    let bytes = AST_SRC.as_bytes();
    let mut depth = 0i32;
    let mut i = enum_start + "pub enum ExprKind {".len() - 1; // start at the `{`
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
        "could not locate end of `pub enum ExprKind` block in ast.rs"
    );
    let body = &AST_SRC[enum_start..enum_end];

    let mut out = BTreeSet::new();
    // Scan line by line. A variant head is a line at depth 1 (one `{`
    // deep from `pub enum`) that starts with an identifier followed by
    // `(`, `{`, or `,`. We track brace depth ourselves to avoid
    // confusing field bodies (depth > 1) with variant heads.
    let mut d = 0i32;
    let mut variants_seen = BTreeSet::new();
    let mut current_line_start_depth = 0i32;
    let mut line_buf = String::new();
    for (_, c) in body.char_indices() {
        if c == '\n' {
            // Inspect `line_buf` at the depth at line start.
            if current_line_start_depth == 1 {
                let trimmed = line_buf.trim();
                if !trimmed.is_empty()
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with("/*")
                    && !trimmed.starts_with("*")
                {
                    // Identifier must come first.
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
                        // Must be followed by `(`, `{`, `,`, or whitespace+such.
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
        // Track depth before pushing so `current_line_start_depth` reflects
        // the depth AT the moment the previous newline ended.
        if c == '{' {
            d += 1;
        } else if c == '}' {
            d -= 1;
        }
        line_buf.push(c);
    }
    // Flush the last line (in case file has no trailing newline before `}`).
    if current_line_start_depth == 1 {
        let trimmed = line_buf.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
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
                variants_seen.insert(first_token);
            }
        }
    }
    out.extend(variants_seen);
    out
}

/// Parse every `ExprKind::Variant` mention inside the body of
/// `visit_expr_children` in `src/lsp/ast_walk.rs`.
fn walker_variant_mentions() -> BTreeSet<String> {
    let fn_idx = WALK_SRC
        .find("fn visit_expr_children(")
        .expect("ast_walk.rs no longer contains `fn visit_expr_children(` — has it moved?");
    // Find the body open `{` after the signature.
    let after = &WALK_SRC[fn_idx..];
    let body_open = after.find('{').expect("visit_expr_children has no body");
    let bytes = after.as_bytes();
    let mut depth = 0i32;
    let mut i = body_open;
    let mut body_end = body_open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = i + 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let body = &after[body_open..body_end];

    let mut out = BTreeSet::new();
    let needle = "ExprKind::";
    let mut search = body;
    while let Some(pos) = search.find(needle) {
        let after_prefix = &search[pos + needle.len()..];
        let variant: String = after_prefix
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !variant.is_empty() {
            out.insert(variant);
        }
        search = &search[pos + needle.len()..];
    }
    out
}

/// Returns the body of `visit_expr_children` so individual tests can
/// inspect it (e.g. to check for `_ => {}`).
fn walker_body() -> &'static str {
    let fn_idx = WALK_SRC
        .find("fn visit_expr_children(")
        .expect("ast_walk.rs no longer contains `fn visit_expr_children(`");
    let after = &WALK_SRC[fn_idx..];
    let body_open = after.find('{').expect("no body open brace");
    let bytes = after.as_bytes();
    let mut depth = 0i32;
    let mut i = body_open;
    let mut body_end = body_open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = i + 1;
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    &after[body_open..body_end]
}

/// Leaf variants that legitimately have no child `Expr` to recurse into.
/// If any of these grow children later, they MUST be moved out of this
/// list and added to `visit_expr_children`.
fn known_leaf_variants() -> BTreeSet<String> {
    [
        "Int",       // i64 literal
        "Float",     // f64 literal
        "Bool",      // bool literal
        "StringLit", // String literal (raw bytes, no embedded exprs)
        "Ident",     // bare identifier
        "Unit",      // ()
        "Return",    // Return(Option<Box<Expr>>) — the `Some` arm IS walked;
                     // listing here covers the `None` case which has no children.
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn every_exprkind_variant_is_either_walked_or_a_known_leaf() {
    let variants = exprkind_variants();
    let walked = walker_variant_mentions();
    let leaves = known_leaf_variants();

    assert!(
        !variants.is_empty(),
        "failed to parse any ExprKind variants from ast.rs — parser changed shape"
    );
    assert!(
        !walked.is_empty(),
        "failed to parse any ExprKind mentions from visit_expr_children — \
         function structure changed"
    );

    let mut missing: Vec<String> = Vec::new();
    for v in &variants {
        if walked.contains(v) {
            continue;
        }
        if leaves.contains(v) {
            continue;
        }
        missing.push(v.clone());
    }
    assert!(
        missing.is_empty(),
        "ExprKind variant(s) {missing:?} are NOT walked by `visit_expr_children` \
         in src/lsp/ast_walk.rs, and NOT listed as known leaves in this test's \
         `known_leaf_variants()`. Either:\n  \
         (1) add an arm to `visit_expr_children` that recurses into the variant's \
         child expression(s) — LSP find-references / hover / inlay-hints all use \
         this walker, so omission is a silent functional regression; OR\n  \
         (2) confirm the variant has no `Expr` children and add it to \
         `known_leaf_variants()` in this test. \n\
         Detected ExprKind variants:   {variants:?}\n\
         Detected walker mentions:     {walked:?}"
    );
}

#[test]
fn walker_does_not_mention_phantom_variants() {
    // Inverse direction: every `ExprKind::*` arm in the walker must
    // correspond to a real variant in `ast.rs`. Catches dead code from
    // renamed/removed variants.
    let variants = exprkind_variants();
    let walked = walker_variant_mentions();
    let mut phantoms: Vec<String> = Vec::new();
    for v in &walked {
        if !variants.contains(v) {
            phantoms.push(v.clone());
        }
    }
    assert!(
        phantoms.is_empty(),
        "`visit_expr_children` references ExprKind variant(s) {phantoms:?} that \
         no longer exist in `pub enum ExprKind` in src/ast.rs. The walker arm is \
         dead code — remove it. Live variants: {variants:?}"
    );
}

#[test]
fn walker_has_no_catchall_arm() {
    // Round-85 G3 lock: `visit_expr_children` MUST NOT contain a
    // `_ => {}` catch-all. The match is now exhaustive over
    // `ExprKind`, so the Rust compiler enforces parity at compile time
    // (adding a new variant without an arm fails `cargo build`). A
    // future edit that re-introduces `_ => {}` would silently re-open
    // the gap — this test guards against that regression.
    //
    // The other tests in this file (`every_exprkind_variant_is_either_walked_or_a_known_leaf`,
    // `walker_does_not_mention_phantom_variants`, `known_leaf_list_only_contains_real_variants`)
    // are retained as belt-and-suspenders cross-checks against the
    // source text in case the leaf-arm pattern is renamed or
    // refactored in a way that bypasses the compiler check.
    let body = walker_body();
    let has_catchall = body.contains("_ => {}") || body.contains("_ => {\n");
    assert!(
        !has_catchall,
        "`visit_expr_children` re-introduced a `_ => {{}}` catch-all arm. \
         This silently regresses LSP find-references / hover / inlay-hints \
         whenever a new `ExprKind` variant is added without a walker arm. \
         Remove the wildcard and enumerate every variant explicitly — \
         leaf variants (literals, Ident, Unit, Return(None)) should be \
         listed under an empty `... => {{}}` arm so future additions \
         force a compile error rather than a silent regression."
    );
}

#[test]
fn known_leaf_list_only_contains_real_variants() {
    // Defensive: the leaf allow-list above must only name variants that
    // actually exist in ExprKind. Catches typos in the allow-list that
    // would silently let a missing arm through.
    let variants = exprkind_variants();
    let leaves = known_leaf_variants();
    for leaf in &leaves {
        assert!(
            variants.contains(leaf),
            "`known_leaf_variants()` lists `{leaf}` but no such variant exists in \
             `pub enum ExprKind` in src/ast.rs. Either fix the typo or remove the \
             stale entry. Live variants: {variants:?}"
        );
    }
}
