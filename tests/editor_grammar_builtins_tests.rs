//! Regression lock: every builtin free-function name listed by
//! `src/module.rs::builtin_free_function_names` (the authoritative source
//! of truth for the `print`/`println`/`panic` trio) must appear in both
//! editor syntax-highlighting grammars, and neither grammar may list a
//! stray builtin not present in that helper.
//!
//! GAP lock (round 95 follow-up): `tests/builtin_free_function_parity_tests.rs`
//! already locks `builtin_free_function_names()` against the typechecker
//! registry, REPL completion, LSP completion, and the GLOBALS_MD docs
//! surface — but NOT the two editor grammars. The trio is hand-encoded in:
//!   - editors/vim/syntax/silt.vim
//!     (`syntax keyword siltBuiltin print println panic`)
//!   - editors/vscode/syntaxes/silt.tmLanguage.json
//!     (`"builtins"` entry: `"match": "\\b(print|println|panic)\\b"`)
//! Adding or removing a builtin free function (e.g. a future `assert` or
//! `debug`) updates the helper + its 4 locked consumers but would silently
//! leave both grammars stale — exactly the drift the round-59/60/72/79
//! constructor & primitive locks were created to prevent.
//!
//! Mirrors the pattern of:
//!   - tests/editor_grammar_constructors_tests.rs
//!   - tests/editor_grammar_primitives_tests.rs
//!
//! If this test fails after changing `builtin_free_function_names()`, add
//! or remove the name in both grammar files above.

use silt::module::builtin_free_function_names;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Returns every line from the vim grammar that declares a `siltBuiltin`
/// keyword group, concatenated. Panics if no such line exists — that is
/// itself a regression.
fn vim_builtin_scope(vim: &str) -> String {
    let lines: Vec<&str> = vim
        .lines()
        .filter(|l| l.contains("siltBuiltin") && l.contains("syntax keyword"))
        .collect();
    assert!(
        !lines.is_empty(),
        "editors/vim/syntax/silt.vim must contain at least one \
         `syntax keyword siltBuiltin ...` line listing the builtin \
         free-function names — this regression-lock test needs it."
    );
    lines.join("\n")
}

/// Returns the portion of the VS Code grammar JSON that defines the
/// `builtins` repository entry (from the `"builtins"` key through the
/// closing brace of that object). A naive forward scan to the first `}`
/// is sufficient because the `"match"` value is a single-line regex
/// without nested braces.
fn vscode_builtins_block(vscode: &str) -> String {
    let marker = "\"builtins\"";
    let start = vscode.find(marker).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \"builtins\" \
         repository entry listing builtin free-function names — this regression-lock test needs it.",
    );
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"builtins\"` entry is malformed: no closing brace found");
    tail[..=end_rel].to_string()
}

/// Does `grammar` mention `name` as a whole identifier (flanked by
/// non-word characters on both sides)? This avoids false positives like
/// `print` matching `println`, while remaining agnostic to the specific
/// regex-alternation punctuation used by each grammar.
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

#[test]
fn editor_grammars_include_all_builtin_free_functions() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    // Narrow to the regions that actually define the builtin
    // alternation/keyword list, so unrelated mentions of e.g. `print`
    // elsewhere in the grammar (in a comment) cannot mask a removal.
    let vim_scope = vim_builtin_scope(&vim_raw);
    let vscode_scope = vscode_builtins_block(&vscode_raw);

    let mut missing: Vec<String> = Vec::new();

    for &name in builtin_free_function_names() {
        if !grammar_mentions_name(&vim_scope, name) {
            missing.push(format!(
                "editors/vim/syntax/silt.vim (siltBuiltin keyword list) is missing \
                 builtin free function `{}`",
                name
            ));
        }
        if !grammar_mentions_name(&vscode_scope, name) {
            missing.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json (\"builtins\" entry) is \
                 missing builtin free function `{}`",
                name
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "Editor syntax grammars are out of sync with \
         src/module.rs::builtin_free_function_names.\n\
         Add the following builtin name(s) to the grammar file(s) listed:\n  - {}\n\
         Authoritative source: src/module.rs (builtin_free_function_names).",
        missing.join("\n  - ")
    );
}

/// Extracts whitespace-delimited tokens from each
/// `syntax keyword siltBuiltin ...` line in the vim scope, stripping the
/// leading `syntax keyword siltBuiltin` prefix on each line. Mirrors
/// `vim_constructor_tokens` in tests/editor_grammar_constructors_tests.rs.
fn vim_builtin_tokens(vim_scope: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for line in vim_scope.lines() {
        // Strip vim line-comment tail (vim uses `"`); the keyword
        // declaration lines themselves never contain a `"`.
        let code = line.split('"').next().unwrap_or("").trim();
        for tok in code
            .split_whitespace()
            .skip_while(|t| *t != "siltBuiltin")
            .skip(1)
        {
            tokens.insert(tok.to_string());
        }
    }
    tokens
}

/// Extracts the alternation tokens from the VS Code `"builtins"` block.
/// In the JSON source the regex anchors appear as `\\b(` … `)\\b` (the
/// backslashes are JSON-escaped), so on the Rust side we match the
/// literal 4-byte sequence `\\b(` written as `"\\\\b("`. Mirrors
/// `vscode_constructor_tokens` in
/// tests/editor_grammar_constructors_tests.rs.
fn vscode_builtin_tokens(block: &str) -> BTreeSet<String> {
    let open_anchor = "\\\\b(";
    let close_anchor = ")\\\\b";
    let open = block
        .find(open_anchor)
        .expect("VS Code \"builtins\" block missing `\\\\b(` opening anchor in JSON source");
    let rest = &block[open + open_anchor.len()..];
    let close = rest
        .find(close_anchor)
        .expect("VS Code \"builtins\" block missing `)\\\\b` closing anchor in JSON source");
    rest[..close]
        .split('|')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Authoritative set of builtin free-function names per
/// `src/module.rs::builtin_free_function_names`.
fn authoritative_builtin_names() -> BTreeSet<String> {
    builtin_free_function_names()
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn every_vim_builtin_entry_is_authoritative() {
    // Reverse direction: catch stale entries left in the vim grammar
    // after a builtin is removed from
    // `src/module.rs::builtin_free_function_names`. The forward test
    // above only catches additions; without this test, removals silently
    // leave ghost highlighting in editors. Mirrors
    // `every_vim_constructor_entry_is_authoritative` in
    // tests/editor_grammar_constructors_tests.rs.
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vim_scope = vim_builtin_scope(&vim_raw);
    let tokens = vim_builtin_tokens(&vim_scope);

    let authoritative = authoritative_builtin_names();

    let mut stray: Vec<String> = Vec::new();
    for tok in &tokens {
        if !authoritative.contains(tok) {
            stray.push(format!(
                "editors/vim/syntax/silt.vim siltBuiltin keyword list contains stray \
                 entry `{}` not in src/module.rs::builtin_free_function_names",
                tok
            ));
        }
    }

    assert!(
        stray.is_empty(),
        "vim grammar lists builtin names not present in \
         builtin_free_function_names. Either remove the stray entries from \
         editors/vim/syntax/silt.vim, or (if the builtin is genuinely new) \
         add it to src/module.rs::builtin_free_function_names first:\n  - {}",
        stray.join("\n  - ")
    );
}

#[test]
fn every_vscode_builtin_entry_is_authoritative() {
    // Reverse direction: catch stale entries left in the VS Code grammar
    // after a builtin is removed from
    // `src/module.rs::builtin_free_function_names`. Mirrors
    // `every_vscode_constructor_entry_is_authoritative` in
    // tests/editor_grammar_constructors_tests.rs.
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");
    let vscode_scope = vscode_builtins_block(&vscode_raw);
    let tokens = vscode_builtin_tokens(&vscode_scope);

    let authoritative = authoritative_builtin_names();

    let mut stray: Vec<String> = Vec::new();
    for tok in &tokens {
        if !authoritative.contains(tok) {
            stray.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json \"builtins\" alternation \
                 contains stray entry `{}` not in src/module.rs::builtin_free_function_names",
                tok
            ));
        }
    }

    assert!(
        stray.is_empty(),
        "VS Code grammar lists builtin names not present in \
         builtin_free_function_names. Either remove the stray entries from \
         editors/vscode/syntaxes/silt.tmLanguage.json, or (if the builtin \
         is genuinely new) add it to \
         src/module.rs::builtin_free_function_names first:\n  - {}",
        stray.join("\n  - ")
    );
}
