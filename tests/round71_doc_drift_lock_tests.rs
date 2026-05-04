//! Round-71 DOC drift locks.
//!
//! These pin three audit-flagged BROKEN findings as source-grep locks
//! against `docs/language/*.md`. The bug class — "documented snippet
//! contradicts the implementation" — is best caught with bad-string
//! locks: we assert that the offending text is no longer present in
//! the doc tree.
//!
//! - DOC-1: `docs/language/generics.md` previously called
//!   `map.insert(...)` in the user-defined-generic-container example.
//!   The actual builtin is `map.set` (registered in
//!   `src/typechecker/builtins/map.rs`), so the snippet would refuse
//!   to typecheck if the walker ever reached it. Lock that no fence
//!   in this file uses `map.insert` again.
//!
//! - DOC-2: `docs/language/traits.md` redeclared the built-in
//!   `Display` trait twice (the default-method demo and the override
//!   demo). The compiler rejects this with
//!   "trait 'Display' is a builtin trait and cannot be redefined",
//!   so neither snippet could compile. Both have been renamed to a
//!   fresh trait name (`trait Show`). Lock that the doc no longer
//!   contains a `trait Display { fn show` or `fn debug` redeclaration
//!   pattern. The bare `trait Display` token *does* legitimately
//!   appear in `trait Display for X { ... }` impl blocks, so the
//!   pattern must be tighter than the bare name.
//!
//! - DOC-3: `docs/language/design-decisions.md` and
//!   `docs/language/loops-and-pipes.md` claimed string `+` was not
//!   supported and that interpolation was the *only* way to build
//!   strings. In fact the typechecker accepts `+` on strings (see
//!   `is_valid_arith_operand` in `src/typechecker/inference.rs`), and
//!   `docs/language/types.md` already uses `p.first + " " + p.last`
//!   in a walker-tested example. Lock that the false claim and the
//!   anti-pattern wording are gone from both pages.

const GENERICS_MD: &str = include_str!("../docs/language/generics.md");
const TRAITS_MD: &str = include_str!("../docs/language/traits.md");
const DESIGN_DECISIONS_MD: &str = include_str!("../docs/language/design-decisions.md");
const LOOPS_AND_PIPES_MD: &str = include_str!("../docs/language/loops-and-pipes.md");

// ────────────────────────────────────────────────────────────────────
// DOC-1: generics.md must not call `map.insert`
// ────────────────────────────────────────────────────────────────────

#[test]
fn generics_md_does_not_call_map_insert() {
    assert!(
        !GENERICS_MD.contains("map.insert"),
        "generics.md must not reference `map.insert` — the actual \
         builtin is `map.set` (registered in \
         src/typechecker/builtins/map.rs). The pre-fix doc snippet \
         would not typecheck."
    );
}

// ────────────────────────────────────────────────────────────────────
// DOC-2: traits.md must not redeclare the built-in `Display` trait
// ────────────────────────────────────────────────────────────────────

#[test]
fn traits_md_does_not_redeclare_builtin_display_trait() {
    // The bare token `trait Display` is fine inside impl blocks
    // (`trait Display for Item { ... }`). The redeclaration has the
    // shape `trait Display {` followed (later in the same block) by
    // either `fn show(self)` or `fn debug(self)` — neither of those
    // is a method on the built-in Display trait, which only declares
    // `display(self) -> String`.
    //
    // Walk the file fence-by-fence and reject any silt-fenced block
    // that opens with `trait Display {` (no `for`!) and contains a
    // `fn show(` or `fn debug(` method. This is tighter than the
    // bare name and won't false-positive on legitimate impl blocks.
    let mut in_fence = false;
    let mut fence_body = String::new();
    let mut violations: Vec<String> = Vec::new();
    for (i, line) in TRAITS_MD.lines().enumerate() {
        let lineno = i + 1;
        if line.trim_start().starts_with("```silt") {
            in_fence = true;
            fence_body.clear();
            continue;
        }
        if in_fence && line.trim_start().starts_with("```") {
            // Close fence — inspect body.
            let opens_decl = fence_body.contains("trait Display {")
                || fence_body.contains("trait Display\n")
                || fence_body.contains("trait Display\r\n");
            let has_show_or_debug =
                fence_body.contains("fn show(self)") || fence_body.contains("fn debug(self)");
            if opens_decl && has_show_or_debug {
                violations.push(format!(
                    "fence ending at traits.md:{lineno} redeclares \
                     `trait Display` with `fn show` or `fn debug` \
                     method — `Display` is a built-in trait and \
                     cannot be redefined; only `display(self) -> \
                     String` is its method."
                ));
            }
            in_fence = false;
            fence_body.clear();
            continue;
        }
        if in_fence {
            fence_body.push_str(line);
            fence_body.push('\n');
        }
    }
    assert!(
        violations.is_empty(),
        "traits.md must not redeclare the built-in `Display` trait:\n{}",
        violations.join("\n")
    );
}

// ────────────────────────────────────────────────────────────────────
// DOC-3: design-decisions.md and loops-and-pipes.md must not claim
// silt has no string concatenation operator.
// ────────────────────────────────────────────────────────────────────

#[test]
fn design_decisions_md_no_string_concat_claim_removed() {
    // The old section heading.
    assert!(
        !DESIGN_DECISIONS_MD.contains("No String Concatenation Operator"),
        "design-decisions.md must not contain the section heading \
         `No String Concatenation Operator` — string `+` is \
         supported (see is_valid_arith_operand in \
         src/typechecker/inference.rs)."
    );
    // Lowercase variant — guards against a renamed-but-still-wrong
    // section.
    assert!(
        !DESIGN_DECISIONS_MD.contains("no string concatenation operator"),
        "design-decisions.md must not contain the prose claim \
         `no string concatenation operator`."
    );
    // The anti-pattern wording that anchored the false claim.
    assert!(
        !DESIGN_DECISIONS_MD.contains("\"hello \" + name + \"!\""),
        "design-decisions.md must not call `\"hello \" + name + \"!\"` \
         an anti-pattern — string `+` is the supported builder."
    );
    // Stronger absolute claim: "is the only inline way" wording.
    assert!(
        !DESIGN_DECISIONS_MD.contains("is the only inline way"),
        "design-decisions.md must not claim interpolation is the \
         only inline way to build strings — string `+` is also \
         supported."
    );
}

#[test]
fn loops_and_pipes_md_no_string_concat_claim_removed() {
    assert!(
        !LOOPS_AND_PIPES_MD.contains("No string concatenation operator exists"),
        "loops-and-pipes.md must not claim no string concatenation \
         operator exists — `+` on strings is supported."
    );
    assert!(
        !LOOPS_AND_PIPES_MD.contains("no string concatenation operator"),
        "loops-and-pipes.md must not contain the prose claim `no \
         string concatenation operator`."
    );
    assert!(
        !LOOPS_AND_PIPES_MD.contains("\"hello \" + name + \"!\""),
        "loops-and-pipes.md must not call `\"hello \" + name + \"!\"` \
         an anti-pattern — string `+` is the supported builder."
    );
    assert!(
        !LOOPS_AND_PIPES_MD.contains("is the only inline way to build strings"),
        "loops-and-pipes.md must not claim interpolation is the only \
         inline way to build strings — string `+` is also supported."
    );
}
