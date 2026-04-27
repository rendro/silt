//! Regression lock: every silt *core* keyword listed in
//! `tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS` must
//! appear in both editor syntax-highlighting grammars. Without this
//! lock a new keyword added to the lexer + the three Rust surfaces
//! (src/lsp/completion.rs, src/lsp/rename.rs, src/repl.rs — all
//! already parity-locked) would still silently fail to highlight in
//! vim / VS Code.
//!
//! This test also rejects stray keywords in either editor file that
//! are NOT in `EXPECTED_CORE_KEYWORDS` — locking the set both ways.
//! Removing a keyword from the editor file without updating the
//! expected list (or vice-versa) trips the regression.
//!
//! Mirrors the pattern of:
//!   - tests/editor_grammar_primitives_tests.rs
//!   - tests/editor_grammar_constructors_tests.rs
//!   - tests/editor_grammar_modules_tests.rs
//!
//! If this test fails after adding a new keyword to silt's lexer,
//! add the keyword name to:
//!   - tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS
//!   - the parallel `EXPECTED_CORE_KEYWORDS` below (duplicated
//!     intentionally — see comment)
//!   - editors/vim/syntax/silt.vim           (siltKeyword list)
//!   - editors/vscode/syntaxes/silt.tmLanguage.json ("keywords" match)
//!   - src/lsp/completion.rs, src/lsp/rename.rs, src/repl.rs

use std::fs;
use std::path::PathBuf;

/// Duplicated intentionally from
/// `tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS` to
/// avoid cross-test-file coupling (integration test files can't
/// `use super::*` each other). Both copies must be updated together;
/// if they drift, the editor grammars will not lock against the same
/// set as the Rust surfaces. Keep strictly in sync.
const EXPECTED_CORE_KEYWORDS: &[&str] = &[
    "as", "else", "fn", "import", "let", "loop", "match", "mod", "pub", "return", "trait", "type",
    "when", "where",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Returns every line from the vim grammar that declares a
/// `siltKeyword` group, concatenated. Panics if no such line exists —
/// that is itself a regression.
fn vim_keyword_scope(vim: &str) -> String {
    let lines: Vec<&str> = vim
        .lines()
        .filter(|l| l.contains("siltKeyword") && l.contains("syntax keyword"))
        .collect();
    assert!(
        !lines.is_empty(),
        "editors/vim/syntax/silt.vim must contain at least one \
         `syntax keyword siltKeyword ...` line listing the core keywords — \
         this regression-lock test needs it."
    );
    lines.join("\n")
}

/// Returns the portion of the VS Code grammar JSON that defines the
/// `keywords` repository entry (from the `"keywords"` key through the
/// closing brace of that object).
fn vscode_keywords_block(vscode: &str) -> String {
    let marker = "\"keywords\"";
    let start = vscode.find(marker).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \"keywords\" \
         repository entry listing core keywords — this regression-lock test needs it.",
    );
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"keywords\"` entry is malformed: no closing brace found");
    tail[..=end_rel].to_string()
}

/// Does `grammar` mention `name` as a whole identifier (flanked by
/// non-word characters on both sides)?
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

/// Extracts whitespace-delimited tokens from the vim `siltKeyword`
/// line(s), stripping the leading `syntax keyword siltKeyword` prefix.
/// Used to verify no stray keyword slipped into the vim file.
fn vim_keyword_tokens(vim_scope: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in vim_scope.lines() {
        // Strip comment tail, if any (vim uses `"` for comments, but
        // the syntax-keyword lines themselves never contain quotes).
        let code = line.split('"').next().unwrap_or("").trim();
        // Skip the `syntax keyword siltKeyword` header tokens.
        for tok in code
            .split_whitespace()
            .skip_while(|t| *t != "siltKeyword")
            .skip(1)
        {
            out.push(tok.to_string());
        }
    }
    out
}

/// Extracts the alternation tokens from the VS Code `"keywords"`
/// block's regex. In the JSON source file, the regex anchors appear
/// as `\\b(` … `)\\b` (the backslash is JSON-escaped). We therefore
/// match the literal 4-byte sequence `\\b(` on the Rust side, which
/// is written as `"\\\\b("` — i.e. two backslashes, then `b(`.
fn vscode_keyword_tokens(vscode_scope: &str) -> Vec<String> {
    let open_anchor = "\\\\b(";
    let close_anchor = ")\\\\b";
    let open = vscode_scope
        .find(open_anchor)
        .expect("VS Code \"keywords\" block missing `\\\\b(` opening anchor in JSON source");
    let rest = &vscode_scope[open + open_anchor.len()..];
    let close = rest
        .find(close_anchor)
        .expect("VS Code \"keywords\" block missing `)\\\\b` closing anchor in JSON source");
    rest[..close]
        .split('|')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

#[test]
fn editor_grammars_include_all_core_keywords() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    let vim_scope = vim_keyword_scope(&vim_raw);
    let vscode_scope = vscode_keywords_block(&vscode_raw);

    let mut missing: Vec<String> = Vec::new();

    for &kw in EXPECTED_CORE_KEYWORDS {
        if !grammar_mentions_name(&vim_scope, kw) {
            missing.push(format!(
                "editors/vim/syntax/silt.vim (siltKeyword list) is missing \
                 core keyword `{}`",
                kw
            ));
        }
        if !grammar_mentions_name(&vscode_scope, kw) {
            missing.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json (\"keywords\" \
                 entry) is missing core keyword `{}`",
                kw
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "Editor syntax grammars are out of sync with \
         tests/keyword_list_parity_tests.rs::EXPECTED_CORE_KEYWORDS.\n\
         Add the following keyword(s) to the grammar file(s) listed:\n  - {}\n\
         Authoritative source: tests/keyword_list_parity_tests.rs.",
        missing.join("\n  - ")
    );
}

#[test]
fn editor_grammars_have_no_stray_keywords() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    let vim_scope = vim_keyword_scope(&vim_raw);
    let vscode_scope = vscode_keywords_block(&vscode_raw);

    let expected: std::collections::HashSet<&str> =
        EXPECTED_CORE_KEYWORDS.iter().copied().collect();

    let mut stray: Vec<String> = Vec::new();

    for tok in vim_keyword_tokens(&vim_scope) {
        if !expected.contains(tok.as_str()) {
            stray.push(format!(
                "editors/vim/syntax/silt.vim siltKeyword line contains stray \
                 entry `{}` not in EXPECTED_CORE_KEYWORDS",
                tok
            ));
        }
    }

    for tok in vscode_keyword_tokens(&vscode_scope) {
        if !expected.contains(tok.as_str()) {
            stray.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json \"keywords\" \
                 alternation contains stray entry `{}` not in \
                 EXPECTED_CORE_KEYWORDS",
                tok
            ));
        }
    }

    assert!(
        stray.is_empty(),
        "Editor grammars contain keywords not present in \
         EXPECTED_CORE_KEYWORDS. Either remove the stray entries \
         from the grammar files, or (if the keyword is genuinely \
         new) update tests/keyword_list_parity_tests.rs and the \
         parallel copy in this file:\n  - {}",
        stray.join("\n  - ")
    );
}
