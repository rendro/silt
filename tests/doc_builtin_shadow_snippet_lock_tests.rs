//! Regression lock: doc code fences must not bind builtin module names.
//!
//! Finding (LATENT): the triple-quoted-string examples in
//! `docs/language/loops-and-pipes.md` bound `let json = """..."""` and
//! `let regex = """..."""` (and the interpolation example bound
//! `let math = ...`). Pasting those documented snippets into a program
//! makes the compiler emit
//! `warning[compile]: variable 'json' shadows the builtin 'json' module;
//!  use a different name to access 'json.*' functions`
//! (see `warn_if_shadows_module` in `src/compiler/mod.rs`).
//!
//! The existing doc walkers in `tests/examples_check.rs`
//! (`all_doc_fn_main_blocks_type_check`,
//! `test_doc_fn_main_blocks_emit_no_compile_warnings`) only inspect
//! fences that contain `fn main`, so these fragment-style fences shipped
//! the warning silently. This lock closes that hole for
//! `loops-and-pipes.md` by scanning EVERY ```silt fence in the page for
//! a `let <name>` binding whose name is any entry of
//! `silt::module::BUILTIN_MODULES` — so adding a new builtin module
//! later also retroactively protects this page's snippets.

use silt::module::BUILTIN_MODULES;

const LOOPS_AND_PIPES_MD: &str = include_str!("../docs/language/loops-and-pipes.md");

/// Extract every ```silt fenced block as (1-indexed opener line, body).
/// Mirrors `extract_silt_blocks` in `tests/examples_check.rs`.
fn extract_silt_blocks(body: &str) -> Vec<(usize, String)> {
    let mut blocks: Vec<(usize, String)> = Vec::new();
    let mut lines = body.lines().enumerate();
    while let Some((idx, line)) = lines.next() {
        if line.trim_start().starts_with("```silt") {
            let opener_line = idx + 1;
            let mut buf = String::new();
            for (_, content) in lines.by_ref() {
                if content.trim_start().starts_with("```") {
                    break;
                }
                buf.push_str(content);
                buf.push('\n');
            }
            blocks.push((opener_line, buf));
        }
    }
    blocks
}

/// If `line` is a `let <ident> ...` binding, return the bound identifier.
fn let_bound_ident(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("let ")?;
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if end == 0 { None } else { Some(&rest[..end]) }
}

#[test]
fn loops_and_pipes_snippets_do_not_shadow_builtin_modules() {
    let blocks = extract_silt_blocks(LOOPS_AND_PIPES_MD);
    assert!(
        !blocks.is_empty(),
        "docs/language/loops-and-pipes.md should contain ```silt fences; \
         if the fences were removed or renamed, re-aim this lock."
    );

    let mut violations: Vec<String> = Vec::new();
    for (opener_line, body) in &blocks {
        for (i, line) in body.lines().enumerate() {
            if let Some(ident) = let_bound_ident(line)
                && BUILTIN_MODULES.contains(&ident)
            {
                violations.push(format!(
                    "docs/language/loops-and-pipes.md:{} (fence at line {}): \
                     `let {}` shadows the builtin '{}' module: {}",
                    opener_line + i + 1,
                    opener_line,
                    ident,
                    ident,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "```silt snippets in docs/language/loops-and-pipes.md must not bind \
         builtin module names — pasting the documented snippet would emit \
         `warning[compile]: variable '<m>' shadows the builtin '<m>' module`. \
         Rename the binding (e.g. `json` -> `json_text`, `regex` -> \
         `pattern`).\n{}",
        violations.join("\n")
    );
}

/// Positive guard so the walker above cannot pass vacuously: the renamed
/// triple-quoted-string examples must still be present in the page.
#[test]
fn loops_and_pipes_triple_quoted_examples_use_nonshadowing_names() {
    assert!(
        LOOPS_AND_PIPES_MD.contains("let json_text = \"\"\""),
        "loops-and-pipes.md: the triple-quoted JSON example should bind \
         `json_text` (a name that does not shadow the `json` builtin \
         module). If the example was legitimately reworded, update this \
         lock alongside it."
    );
    assert!(
        LOOPS_AND_PIPES_MD.contains("let pattern = \"\"\""),
        "loops-and-pipes.md: the triple-quoted regex example should bind \
         `pattern` (a name that does not shadow the `regex` builtin \
         module). If the example was legitimately reworded, update this \
         lock alongside it."
    );
}
