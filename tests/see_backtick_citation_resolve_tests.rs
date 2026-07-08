//! Resolve-lock for prose `see `ident`` citations in doc-comments.
//!
//! Round-4 (`0672a35`) fixed a doc-comment in `src/typechecker/inference.rs`
//! that cited a function (`type_name_for_method_dispatch`) which never
//! existed, and added `tests/typechecker_citation_resolve_tests.rs` — but
//! that lock is scoped to `src/typechecker/mod.rs:<N>` line cites made from
//! `inference.rs`/`dispatch.rs`. The same bug class existed elsewhere and
//! escaped it:
//!
//!   * `src/builtins/postgres.rs` (`do_listen_worker`'s doc-comment) cited
//!     a phantom `do_listen_spawn`; the LISTEN statement is actually issued
//!     inside `listen`'s io-pool closure before the worker is spawned.
//!   * `src/formatter.rs` (fn-type effect-slot comment) cited a phantom
//!     `format_fn_decl`; the real `FnDecl` formatter is
//!     `format_fn_with_comments`.
//!
//! This test scans BOTH files for every ``see `X` `` citation and resolves
//! each one:
//!
//!   * a path cite (contains `/`) must name a file that exists;
//!   * an identifier cite must name a function defined somewhere under
//!     `src/` (``fn <name>(`` / ``fn <name><``) or a local binding in the
//!     citing file (``let <name>``, e.g. formatter's `is_last_on_line`).
//!
//! For `Path::seg` cites only the last segment is resolved — coarse, but
//! enough to catch the "function never existed / was renamed" class.
//! Because the scan is content-based (no line numbers), ordinary edits to
//! either file cannot break this lock; only re-introducing an unresolvable
//! citation can.
//!
//! `include_str!` keeps the scan unconditional even though
//! `src/builtins/postgres.rs` is gated behind `feature = "postgres"` — the
//! file content is read at compile time as a `&str`, not as Rust code.

use std::path::Path;

const POSTGRES_RS: &str = include_str!("../src/builtins/postgres.rs");
const FORMATTER_RS: &str = include_str!("../src/formatter.rs");

/// Extract every backticked citation body following the word "see ",
/// i.e. the `X` of each ``see `X` `` occurrence in `src`.
fn see_cites(src: &str) -> Vec<String> {
    const NEEDLE: &str = "see `";
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(pos) = rest.find(NEEDLE) {
        let after = &rest[pos + NEEDLE.len()..];
        match after.find('`') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break, // unterminated backtick at EOF; nothing to cite
        }
    }
    out
}

/// True iff `name` is a plain Rust path of identifiers:
/// `ident` or `Ident::ident` (any depth).
fn is_ident_path(name: &str) -> bool {
    !name.is_empty()
        && name.split("::").all(|seg| {
            let mut chars = seg.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// Concatenate the contents of every `.rs` file under `dir`, recursively.
fn read_all_rs_sources(dir: &Path, out: &mut String) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("readable dir entry").path();
        if path.is_dir() {
            read_all_rs_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display())),
            );
            out.push('\n');
        }
    }
}

/// Every ``see `X` `` citation in `postgres.rs` and `formatter.rs` must
/// resolve to something that exists: a file (path cites), a function
/// defined under `src/` (ident cites), or a local binding in the citing
/// file. Fails on phantom references like the pre-fix `do_listen_spawn`
/// and `format_fn_decl`.
#[test]
fn see_backtick_citations_resolve() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all_src = String::new();
    read_all_rs_sources(&manifest_dir.join("src"), &mut all_src);

    let files: &[(&str, &str)] = &[
        ("src/builtins/postgres.rs", POSTGRES_RS),
        ("src/formatter.rs", FORMATTER_RS),
    ];

    let mut total = 0usize;
    for &(file, src) in files {
        for cite in see_cites(src) {
            total += 1;
            if cite.contains('/') {
                // Path cite, e.g. `src/typechecker/builtins/errors.rs`.
                assert!(
                    manifest_dir.join(&cite).is_file(),
                    "stale citation in {file}: `see \\`{cite}\\`` names a \
                     file that does not exist — re-aim the comment at the \
                     file that replaced it."
                );
                continue;
            }
            assert!(
                is_ident_path(&cite),
                "unrecognised citation form in {file}: `see \\`{cite}\\`` \
                 is neither a file path nor an identifier path — extend \
                 tests/see_backtick_citation_resolve_tests.rs to resolve \
                 this new cite shape."
            );
            let seg = cite.rsplit("::").next().expect("non-empty ident path");
            let is_fn =
                all_src.contains(&format!("fn {seg}(")) || all_src.contains(&format!("fn {seg}<"));
            let is_local_binding = src.contains(&format!("let {seg}"));
            assert!(
                is_fn || is_local_binding,
                "stale citation in {file}: `see \\`{cite}\\`` names an item \
                 with no `fn {seg}` anywhere under src/ and no `let {seg}` \
                 in the citing file. Either the item was renamed/deleted or \
                 it never existed (the round-4 `type_name_for_method_dispatch` \
                 class) — re-aim the comment at the real item."
            );
        }
    }

    // Scanner-rot sanity: this file was written when the two sources held
    // 8 `see `X`` cites between them. If the scanner ever returns nothing,
    // the resolve loop above would vacuously pass — require a floor.
    assert!(
        total >= 2,
        "see-cite scanner found only {total} citation(s) across \
         postgres.rs + formatter.rs — expected several; the extraction \
         logic has likely rotted."
    );

    // The phantoms must stay dead, and the re-aimed targets must exist
    // under their real names (mirrors the round-4 lock's tail asserts).
    for &(file, src) in files {
        for phantom in ["do_listen_spawn", "format_fn_decl"] {
            assert!(
                !src.contains(phantom),
                "the phantom function `{phantom}` reappeared in {file} — it \
                 has never existed. LISTEN is issued in `listen`'s io-pool \
                 closure (postgres.rs); the FnDecl formatter is \
                 `format_fn_with_comments` (formatter.rs)."
            );
        }
    }
    assert!(
        POSTGRES_RS.contains("fn listen("),
        "expected `fn listen(` in src/builtins/postgres.rs — the \
         do_listen_worker doc-comment cites it as the site that issues \
         LISTEN before spawning the worker."
    );
    assert!(
        FORMATTER_RS.contains("fn format_fn_with_comments("),
        "expected `fn format_fn_with_comments(` in src/formatter.rs — the \
         fn-type effect-slot comment cites it as the FnDecl formatter."
    );
}
