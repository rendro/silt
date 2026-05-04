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
// DOC-2: doc fences must not redeclare *any* built-in trait
// ────────────────────────────────────────────────────────────────────
//
// Round-72 generalisation of the original DOC-2 lock (which only
// guarded `trait Display`): the typechecker rejects redeclaration of
// any of the five built-ins
// (`src/typechecker/mod.rs::BUILTIN_TRAIT_NAMES`). A `silt`-fenced
// block that opens `trait <Name> {` (no `for` — that's an impl) for
// any of those names cannot compile and is a doc bug. Round-71's fix
// renamed `Display` -> `Show`; round 72's audit caught the same
// pattern with `Equal` / `Ordered` (Ordered being a hand-rolled
// supertrait demo whose name shadowed nothing, but whose `Equal`
// supertrait collided with the built-in). The rename in
// `traits.md` to `Eq2` / `Cmp2` removes both collisions.
//
// We walk every `.md` file under `docs/` (recursive) plus `README.md`,
// extract every `silt`-fenced block, and reject any opening line
// matching `trait <Built-in> {` or `trait <Built-in>:` (supertrait
// declaration form). Impl blocks (`trait Display for Color { ... }`)
// remain allowed — they don't *redefine* the trait.
//
// We deliberately do NOT scan `docs/proposals/*.md` because proposals
// may reference future or hypothetical trait shapes during design
// discussion.

#[test]
fn no_doc_fence_redeclares_a_builtin_trait() {
    use std::path::{Path, PathBuf};
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Match `src/typechecker/mod.rs::BUILTIN_TRAIT_NAMES` exactly.
    const BUILTINS: &[&str] = &["Equal", "Compare", "Hash", "Display", "Error"];

    let mut targets: Vec<PathBuf> = Vec::new();
    let readme = manifest_dir.join("README.md");
    if readme.is_file() {
        targets.push(readme);
    }
    fn collect_md(dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                // Skip proposals/ — they may discuss hypothetical
                // trait shapes during design.
                if p.file_name().and_then(|s| s.to_str()) == Some("proposals") {
                    continue;
                }
                collect_md(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("md") {
                out.push(p);
            }
        }
    }
    collect_md(&manifest_dir.join("docs"), &mut targets);
    targets.sort();

    let mut violations: Vec<String> = Vec::new();

    for path in &targets {
        let body = std::fs::read_to_string(path).expect("read doc");
        let mut in_fence = false;
        let mut fence_open_line = 0usize;
        let mut fence_body = String::new();
        for (i, line) in body.lines().enumerate() {
            let lineno = i + 1;
            if !in_fence && line.trim_start().starts_with("```silt") {
                in_fence = true;
                fence_open_line = lineno;
                fence_body.clear();
                continue;
            }
            if in_fence && line.trim_start().starts_with("```") {
                // Close — scan fence_body for any built-in
                // redeclaration.
                for name in BUILTINS {
                    // Open-brace form: `trait Equal {` (whole-word).
                    let needle_brace = format!("trait {name} {{");
                    // Supertrait form: `trait Equal: Foo` is a
                    // redeclaration of `Equal` even without a body
                    // brace yet.
                    let needle_super = format!("trait {name}:");
                    // Newline-separated body (declaration with no
                    // brace on the same line).
                    let needle_nl = format!("trait {name}\n");
                    if fence_body.contains(&needle_brace)
                        || fence_body.contains(&needle_super)
                        || fence_body.contains(&needle_nl)
                    {
                        violations.push(format!(
                            "{}:{} (```silt fence): redeclares \
                             built-in trait `{}` — built-in traits \
                             (`Equal`, `Compare`, `Hash`, `Display`, \
                             `Error`) cannot be redefined per \
                             `src/typechecker/mod.rs::BUILTIN_TRAIT_NAMES`. \
                             Rename the local trait or use a \
                             distinct name.",
                            path.display(),
                            fence_open_line,
                            name
                        ));
                    }
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
    }

    assert!(
        violations.is_empty(),
        "doc fences must not redeclare any built-in trait:\n{}",
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
