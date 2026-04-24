//! Regression lock: every builtin enum variant listed by
//! `src/module.rs::builtin_enum_variants` (the authoritative source of
//! truth for constructors registered by silt's builtin modules) must
//! appear in both editor syntax-highlighting grammars.
//!
//! If this test fails after adding a new builtin constructor, add the
//! constructor name to:
//!   - editors/vim/syntax/silt.vim
//!     (siltConstructor keyword list)
//!   - editors/vscode/syntaxes/silt.tmLanguage.json
//!     ("constructors" match alternation)
//!
//! Round-59 GAP lock: the vast majority of gated constructors
//! (Sent, Recv, Send, Monday..Sunday, GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS,
//! and all typed `*Error` variants) were absent from both grammars until
//! this test was introduced. Do not loosen this check — it is the
//! mechanism that keeps the grammars honest.

use silt::module::builtin_enum_variants;

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Returns every line from the vim grammar that defines a
/// `siltConstructor` keyword group, concatenated.
/// Panics if no such line exists — that is itself a regression.
fn vim_constructor_scope(vim: &str) -> String {
    let lines: Vec<&str> = vim
        .lines()
        .filter(|l| l.contains("siltConstructor") && l.contains("syntax keyword"))
        .collect();
    assert!(
        !lines.is_empty(),
        "editors/vim/syntax/silt.vim must contain at least one \
         `syntax keyword siltConstructor ...` line listing the builtin \
         constructor names — this regression-lock test needs it."
    );
    lines.join("\n")
}

/// Returns the portion of the VS Code grammar JSON that defines the
/// `constructors` repository entry (from the `"constructors"` key
/// through the closing brace of that object). A naive forward scan to
/// the first `}` is sufficient because the `"match"` value is a
/// single-line regex without nested braces.
fn vscode_constructors_block(vscode: &str) -> String {
    let marker = "\"constructors\"";
    let start = vscode.find(marker).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \"constructors\" \
         repository entry listing builtin constructor names — this regression-lock test needs it.",
    );
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"constructors\"` entry is malformed: no closing brace found");
    tail[..=end_rel].to_string()
}

/// Does `grammar` mention `name` as a whole identifier (flanked by
/// non-word characters on both sides)? This avoids false positives
/// like `Send` matching `Sent`, while remaining agnostic to the
/// specific regex-alternation punctuation used by each grammar.
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
fn editor_grammars_include_all_builtin_constructors() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    // Narrow to the regions that actually define the constructor
    // alternation/keyword list, so unrelated mentions of e.g. `Send`
    // elsewhere in the grammar cannot mask a removal.
    let vim_scope = vim_constructor_scope(&vim_raw);
    let vscode_scope = vscode_constructors_block(&vscode_raw);

    let mut missing: Vec<String> = Vec::new();

    for (_enum_name, variants) in builtin_enum_variants() {
        for &variant in *variants {
            if !grammar_mentions_name(&vim_scope, variant) {
                missing.push(format!(
                    "editors/vim/syntax/silt.vim (siltConstructor keyword list) is missing \
                     builtin constructor `{}`",
                    variant
                ));
            }
            if !grammar_mentions_name(&vscode_scope, variant) {
                missing.push(format!(
                    "editors/vscode/syntaxes/silt.tmLanguage.json (\"constructors\" entry) is \
                     missing builtin constructor `{}`",
                    variant
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "Editor syntax grammars are out of sync with \
         src/module.rs::builtin_enum_variants.\n\
         Add the following constructor name(s) to the grammar file(s) listed:\n  - {}\n\
         Authoritative source: src/module.rs (builtin_enum_variants).",
        missing.join("\n  - ")
    );
}
