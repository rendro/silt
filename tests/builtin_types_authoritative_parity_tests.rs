//! Bidirectional parity locks for the authoritative built-in type
//! table at `silt::types::builtins::BUILTIN_TYPES`.
//!
//! These tests guard the four hand-rolled mirrors that historically
//! drifted from the authoritative list:
//!
//! 1. The typechecker arity / kind classification at
//!    `src/typechecker/mod.rs::check_trait_impl` — the table is
//!    derived directly from `BUILTIN_TYPES` so a code-level test
//!    suffices (`is_builtin_container_matches_authoritative_kind`).
//! 2. The LSP rename guard `src/lsp/rename::builtin_globals()` —
//!    `lsp_rename_protects_every_authoritative_name` asserts every
//!    authoritative name is rejected by `is_user_renameable`.
//! 3. The two editor-grammar text files
//!    (`editors/vim/syntax/silt.vim` and
//!    `editors/vscode/syntaxes/silt.tmLanguage.json`) — these are not
//!    derivable at runtime, so
//!    `every_authoritative_name_appears_in_editor_grammars` reads
//!    them at test-time and asserts both directions of the set match
//!    (every authoritative name appears in both grammars; no name
//!    appears in either grammar's type-keyword scope without being in
//!    the authoritative table).
//!
//! Adding a new entry to `BUILTIN_TYPES` without also updating the
//! two editor grammars will fail
//! `every_authoritative_name_appears_in_editor_grammars`. Removing an
//! entry from `BUILTIN_TYPES` without also removing it from the
//! grammars will fail the same test in the reverse direction.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use silt::types::builtins::{self, BUILTIN_TYPES, BuiltinKind};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Whole-word membership check matching the helper in
/// `tests/editor_grammar_primitives_tests.rs`.
fn grammar_mentions_name(grammar: &str, name: &str) -> bool {
    let bytes = grammar.as_bytes();
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

/// Extract the `siltType` keyword line(s) from the vim grammar.
fn vim_type_scope(vim: &str) -> String {
    vim.lines()
        .filter(|l| l.contains("siltType") && l.contains("syntax keyword"))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Extract the `"primitives"` block from the VS Code grammar.
fn vscode_primitives_block(vscode: &str) -> String {
    let marker = "\"primitives\"";
    let start = vscode
        .find(marker)
        .expect("VS Code grammar must contain a \"primitives\" entry");
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"primitives\"` entry malformed: no closing brace");
    tail[..=end_rel].to_string()
}

/// Pull the alternation tokens (e.g. `Int`, `Float`, ...) out of the
/// VS Code primitives `"match"` regex. The regex is a single line of
/// the form `"\\b(A|B|C|...)\\b"`, so a naive split on `|` between the
/// first `(` and last `)` recovers the token set.
fn vscode_primitive_tokens(block: &str) -> BTreeSet<String> {
    let match_pos = block.find("\"match\"").expect("primitives block has \"match\"");
    let after_match = &block[match_pos..];
    let open = after_match.find('(').expect("match regex has open paren");
    let close = after_match[open..]
        .find(')')
        .expect("match regex has close paren");
    let inner = &after_match[open + 1..open + close];
    inner
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pull the keyword tokens out of the vim `siltType` keyword line.
/// Format: `syntax keyword siltType Int Float ExtFloat Bool ...`.
fn vim_type_tokens(line: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut after_silt_type = false;
    for tok in line.split_whitespace() {
        if after_silt_type {
            tokens.insert(tok.to_string());
        }
        if tok == "siltType" {
            after_silt_type = true;
        }
    }
    tokens
}

/// Authoritative set of names that must appear in editor grammars.
/// `()` is the punctuation surface for `Unit` and is not represented
/// as a keyword in either grammar — exclude it.
fn authoritative_grammar_names() -> BTreeSet<String> {
    builtins::iter_all()
        .map(|b| b.name)
        .filter(|n| *n != "()")
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn every_authoritative_name_appears_in_editor_grammars() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");
    let vim_scope = vim_type_scope(&vim_raw);
    let vscode_scope = vscode_primitives_block(&vscode_raw);

    let authoritative = authoritative_grammar_names();

    // Direction 1: every authoritative name appears in both grammars.
    let mut missing: Vec<String> = Vec::new();
    for name in &authoritative {
        if !grammar_mentions_name(&vim_scope, name) {
            missing.push(format!("vim siltType missing `{name}`"));
        }
        if !grammar_mentions_name(&vscode_scope, name) {
            missing.push(format!("vscode primitives missing `{name}`"));
        }
    }
    assert!(
        missing.is_empty(),
        "Editor grammars are missing authoritative built-in type names. \
         Add to:\n  - editors/vim/syntax/silt.vim (siltType keyword line)\n  \
         - editors/vscode/syntaxes/silt.tmLanguage.json (\"primitives\" regex)\n\n\
         Missing:\n  - {}",
        missing.join("\n  - ")
    );

    // Direction 2: no token in either grammar's type-keyword scope is
    // outside the authoritative set. Locks the relationship both ways.
    let vim_tokens = vim_type_tokens(&vim_scope);
    let vscode_tokens = vscode_primitive_tokens(&vscode_scope);

    let mut extra: Vec<String> = Vec::new();
    for tok in &vim_tokens {
        if !authoritative.contains(tok) {
            extra.push(format!("vim siltType has `{tok}` (not in BUILTIN_TYPES)"));
        }
    }
    for tok in &vscode_tokens {
        if !authoritative.contains(tok) {
            extra.push(format!(
                "vscode primitives has `{tok}` (not in BUILTIN_TYPES)"
            ));
        }
    }
    assert!(
        extra.is_empty(),
        "Editor grammars list type-keyword tokens not in the authoritative \
         BUILTIN_TYPES table. Either add the name to \
         `src/types/builtins.rs::BUILTIN_TYPES` or remove it from the \
         grammar.\n\nExtra:\n  - {}",
        extra.join("\n  - ")
    );
}

#[test]
fn lookup_returns_correct_arity_for_known_names() {
    assert_eq!(builtins::lookup("Int").and_then(|b| b.arity), Some(0));
    assert_eq!(builtins::lookup("Float").and_then(|b| b.arity), Some(0));
    assert_eq!(builtins::lookup("Bool").and_then(|b| b.arity), Some(0));
    assert_eq!(builtins::lookup("Unit").and_then(|b| b.arity), Some(0));
    assert_eq!(builtins::lookup("()").and_then(|b| b.arity), Some(0));

    assert_eq!(builtins::lookup("List").and_then(|b| b.arity), Some(1));
    assert_eq!(builtins::lookup("Range").and_then(|b| b.arity), Some(1));
    assert_eq!(builtins::lookup("Set").and_then(|b| b.arity), Some(1));
    assert_eq!(builtins::lookup("Channel").and_then(|b| b.arity), Some(1));
    assert_eq!(builtins::lookup("Map").and_then(|b| b.arity), Some(2));

    // Variadic shapes carry `arity: None`.
    assert_eq!(builtins::lookup("Tuple").map(|b| b.arity), Some(None));
    assert_eq!(builtins::lookup("Fn").map(|b| b.arity), Some(None));
    assert_eq!(builtins::lookup("Fun").map(|b| b.arity), Some(None));
    assert_eq!(builtins::lookup("Handle").map(|b| b.arity), Some(None));

    // Unknown names return None outright.
    assert!(builtins::lookup("NotABuiltin").is_none());
}

#[test]
fn is_builtin_container_matches_authoritative_kind() {
    // `is_container` and `is_primitive` are the convenience wrappers
    // the typechecker uses; assert they agree with the kind field on
    // every authoritative entry.
    for entry in BUILTIN_TYPES {
        match entry.kind {
            BuiltinKind::Container => {
                assert!(
                    builtins::is_container(entry.name),
                    "`{}` is BuiltinKind::Container but is_container returned false",
                    entry.name
                );
                assert!(
                    !builtins::is_primitive(entry.name),
                    "`{}` is BuiltinKind::Container but is_primitive returned true",
                    entry.name
                );
            }
            BuiltinKind::Primitive => {
                assert!(
                    builtins::is_primitive(entry.name),
                    "`{}` is BuiltinKind::Primitive but is_primitive returned false",
                    entry.name
                );
                assert!(
                    !builtins::is_container(entry.name),
                    "`{}` is BuiltinKind::Primitive but is_container returned true",
                    entry.name
                );
            }
        }
    }

    // Negative: names not in BUILTIN_TYPES are neither.
    assert!(!builtins::is_container("WidgetName"));
    assert!(!builtins::is_primitive("WidgetName"));
}

#[test]
#[cfg(feature = "lsp")]
fn lsp_rename_protects_every_authoritative_name() {
    for entry in BUILTIN_TYPES {
        assert!(
            !silt::lsp::is_user_renameable(entry.name),
            "LSP rename guard does not protect authoritative name `{}` — \
             `is_user_renameable` should return false but returned true. \
             Check that `src/lsp/rename::builtin_globals()` derives from \
             `crate::types::builtins::iter_all()`.",
            entry.name
        );
    }
}
