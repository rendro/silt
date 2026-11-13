//! Round-71 DOC drift locks.
//!
//! These pin an audit-flagged BROKEN finding as source-grep locks
//! against `docs/language/*.md`. The bug class — "documented snippet
//! contradicts the implementation" — is best caught with bad-string
//! locks: we assert that the offending text is no longer present in
//! the doc tree.
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
