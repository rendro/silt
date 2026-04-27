//! Round-58 parity lock: every gated enum constructor listed in
//! `src/module.rs::builtin_enum_variants` (the authoritative source of
//! truth for constructors registered by silt's builtin modules) must
//! be recognized by each editor-facing surface that consults builtin
//! lists:
//!
//!   * LSP rename (`src/lsp/rename.rs`) — must reject renames that
//!     target any gated constructor (otherwise `silt rename` corrupts
//!     user programs that call stdlib APIs).
//!   * LSP completion (`src/lsp/completion.rs`) — bare completion list
//!     should offer every constructor, including gated ones.
//!   * REPL completion (`src/repl.rs`) — tab-completion must suggest
//!     every constructor.
//!
//! Mirrors the style of `tests/editor_grammar_constructors_tests.rs`:
//! scan the source files for the hardcoded lists (or the authoritative
//! helper) and assert every variant is mentioned. If this test fails
//! after adding a new gated constructor, the fix is to route the new
//! surface through `module::all_builtin_constructor_names` rather than
//! re-adding another hand-rolled list.
//!
//! Before the round-58 fix, three separate hardcoded lists tracked
//! gated constructors — they had diverged, breaking rename and
//! autocompletion for the ~50 typed-error variants (IoNotFound,
//! JsonSyntax, PgConnect, …) plus Recv/Send. Do not loosen this check.

use silt::module::{all_builtin_constructor_names, builtin_enum_variants};

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Does `source` mention `name` as a whole identifier (flanked by
/// non-word characters on both sides)? Avoids false positives like
/// `Send` matching `Sent` while remaining agnostic to the surrounding
/// syntax (string literal, iterator, match arm, etc.).
fn source_mentions_name(source: &str, name: &str) -> bool {
    let bytes = source.as_bytes();
    let nlen = name.len();
    let nbytes = name.as_bytes();
    if bytes.len() < nlen {
        return false;
    }
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i + nlen <= bytes.len() {
        if &bytes[i..i + nlen] == nbytes {
            let left_ok = i == 0 || !is_word(bytes[i - 1]);
            let right_ok = i + nlen == bytes.len() || !is_word(bytes[i + nlen]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Collects every constructor variant (prelude + gated) from the
/// authoritative `builtin_enum_variants` registry. Deduplicated because
/// some names appear in more than one enum (e.g. `Closed`/`Empty` are
/// shared between ChannelResult and ChannelError).
fn all_variants() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = all_builtin_constructor_names().collect();
    out.sort();
    out.dedup();
    out
}

// ─── Authoritative helper sanity ──────────────────────────────────────

#[test]
fn all_builtin_constructor_names_matches_enum_variants_flatten() {
    // Belt-and-braces: the helper must be a pure flatten of
    // `builtin_enum_variants`. If someone changes one without the
    // other the parity lock's notion of "authoritative set" silently
    // drifts.
    let from_helper: Vec<&'static str> = all_builtin_constructor_names().collect();
    let from_enums: Vec<&'static str> = builtin_enum_variants()
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .collect();
    assert_eq!(
        from_helper, from_enums,
        "all_builtin_constructor_names must equal flatten(builtin_enum_variants)"
    );
}

#[test]
fn prelude_constructors_present_in_authoritative_set() {
    // Sanity: the helper covers the prelude constructors (the historical
    // "four always-available" set), not just the gated ones.
    let all = all_variants();
    for name in ["Ok", "Err", "Some", "None"] {
        assert!(
            all.contains(&name),
            "expected prelude constructor `{name}` in all_builtin_constructor_names"
        );
    }
}

#[test]
fn gated_constructors_present_in_authoritative_set() {
    // Sanity: the helper covers the gated-error variants that were
    // missing from the hand-rolled lists before round 58.
    let all = all_variants();
    for name in [
        "IoNotFound",
        "JsonSyntax",
        "PgConnect",
        "Recv",
        "Send",
        "Monday",
        "GET",
        "HttpTimeout",
        "BytesInvalidUtf8",
        "ChannelTimeout",
    ] {
        assert!(
            all.contains(&name),
            "expected gated constructor `{name}` in all_builtin_constructor_names"
        );
    }
}

// ─── LSP rename ───────────────────────────────────────────────────────

#[test]
fn lsp_rename_covers_every_gated_constructor() {
    // The rename path protects constructors by asking
    // `module::all_builtin_constructor_names().any(|c| c == name)`. We
    // verify the source routes through that helper (not a stale copy
    // of the hardcoded `BUILTIN_CONSTRUCTORS` list) AND we verify the
    // old hardcoded list is gone — otherwise it could silently come
    // back and shadow the helper call.
    let src = read_source("src/lsp/rename.rs");

    assert!(
        src.contains("all_builtin_constructor_names"),
        "src/lsp/rename.rs must consult \
         `module::all_builtin_constructor_names` so gated variants \
         (IoNotFound/PgConnect/Recv/Send/etc.) are protected from \
         rename. Before round-58 a hardcoded `BUILTIN_CONSTRUCTORS` \
         list here only covered ~half of the gated constructors."
    );
    assert!(
        !src.contains("const BUILTIN_CONSTRUCTORS"),
        "src/lsp/rename.rs should not define a hand-rolled \
         `BUILTIN_CONSTRUCTORS` array — the authoritative list lives \
         in `module::all_builtin_constructor_names`."
    );

    // The phantom `"unreachable"` was never a registered builtin;
    // removing it from BUILTIN_GLOBALS is part of round-58.
    // Find the BUILTIN_GLOBALS block and scan it specifically to avoid
    // false positives from the word `unreachable` elsewhere in the file.
    if let Some(start) = src.find("const BUILTIN_GLOBALS") {
        let tail = &src[start..];
        let end = tail.find("];").unwrap_or(tail.len());
        let globals_block = &tail[..end];
        assert!(
            !source_mentions_name(globals_block, "unreachable"),
            "BUILTIN_GLOBALS must not contain phantom `\"unreachable\"` — \
             it is not a registered builtin (round-58 LATENT fix)."
        );
    }
}

// ─── LSP completion ───────────────────────────────────────────────────

#[test]
fn lsp_completion_covers_every_gated_constructor() {
    // `builtins()` in src/lsp/completion.rs must emit a CONSTRUCTOR
    // entry for every variant. We verify the source uses
    // `all_builtin_constructor_names` (which then flows into the
    // CompletionItem list). Before round-58 the function only
    // hardcoded the 4 prelude constructors.
    let src = read_source("src/lsp/completion.rs");
    assert!(
        src.contains("all_builtin_constructor_names"),
        "src/lsp/completion.rs (`builtins` function) must source \
         constructors from `module::all_builtin_constructor_names` so \
         gated variants (Recv, Send, IoNotFound, PgConnect, Monday, …) \
         appear in bare completion. Before round-58 only the 4 prelude \
         constructors were emitted."
    );

    // Dot-completion must also emit gated constructors for a module
    // prefix. The fix routes through `gated_constructor_module`.
    assert!(
        src.contains("gated_constructor_module"),
        "src/lsp/completion.rs dot-completion must consult \
         `module::gated_constructor_module` so `io.`/`json.`/`http.`/ \
         `channel.`/`postgres.`/`time.`/… offer their gated variants \
         alongside module functions and constants."
    );
}

// ─── REPL completion ──────────────────────────────────────────────────

#[test]
fn repl_builtin_names_covers_every_gated_constructor() {
    // The REPL completion list must include every gated constructor.
    // We call the public `builtin_names` directly — this is the most
    // robust form of the test because it exercises the actual surface
    // the REPL consults at runtime rather than scanning source text.
    let names = silt::repl::builtin_names();
    let mut missing: Vec<&'static str> = Vec::new();
    for variant in all_variants() {
        if !names.iter().any(|n| n == variant) {
            missing.push(variant);
        }
    }
    assert!(
        missing.is_empty(),
        "REPL `builtin_names` is missing gated constructors:\n  - {}\n\
         Fix: source constructors from \
         `module::all_builtin_constructor_names` in \
         `src/repl.rs::builtin_names` rather than hand-rolling the list.",
        missing.join("\n  - ")
    );
}

#[test]
fn repl_builtin_names_contains_a_sample_of_gated_constructors() {
    // Belt-and-braces smoke test. Before round-58 the REPL hardcoded
    // list only included `Stop`, `Continue`, `Message`, `Closed`, `Empty`
    // — these variants below were absent and tab-completion missed them.
    let names = silt::repl::builtin_names();
    for expected in [
        "Sent",
        "Recv",
        "Send",
        "IoNotFound",
        "JsonSyntax",
        "PgConnect",
        "Monday",
        "GET",
        "HttpTimeout",
        "BytesInvalidUtf8",
        "ChannelTimeout",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "REPL `builtin_names` missing gated constructor `{expected}`. \
             Before round-58 the hand-rolled list in src/repl.rs \
             omitted every typed-error variant + Recv/Send/Sent/Monday/etc."
        );
    }
}
