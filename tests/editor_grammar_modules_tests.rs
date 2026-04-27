//! Regression lock: every builtin module listed in `src/module.rs`
//! (the authoritative `BUILTIN_MODULES` constant) must appear in both
//! editor syntax-highlighting grammars.
//!
//! If this test fails after adding a new builtin module, add the
//! module name to:
//!   - editors/vim/syntax/silt.vim       (siltModule keyword alternation)
//!   - editors/vscode/syntaxes/silt.tmLanguage.json ("modules" match)
//!
//! Round-52 GAP lock: 8 modules (toml, postgres, bytes, crypto,
//! encoding, tcp, stream, uuid) were absent from both grammars until
//! this test was introduced. Do not loosen this check — it is the
//! mechanism that keeps the grammars honest.

use silt::module::BUILTIN_MODULES;

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Returns the single line from the vim grammar file that defines the
/// module-name match (identified by the `siltModule` marker).
/// Panics if no such line exists — that is itself a regression.
fn vim_module_line(vim: &str) -> &str {
    vim.lines()
        .find(|l| l.contains("siltModule") && l.contains("match"))
        .expect(
            "editors/vim/syntax/silt.vim must contain a `syntax match siltModule ...` line \
             listing the builtin module names — this regression-lock test needs it.",
        )
}

/// Returns the portion of the VS Code grammar JSON that defines the
/// `modules` repository entry (from the `"modules"` key through the
/// closing brace of that object). We do a conservative scan: find the
/// `"modules"` key, then include everything up to (and including) the
/// first closing `}` that terminates the object body.
fn vscode_modules_block(vscode: &str) -> String {
    let marker = "\"modules\"";
    let start = vscode.find(marker).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \"modules\" repository \
         entry listing builtin module names — this regression-lock test needs it.",
    );
    // Scan forward for the first `}` that closes the object body.
    // A naive scan is sufficient because the `"match"` value is a
    // single-line regex without nested braces.
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"modules\"` entry is malformed: no closing brace found");
    tail[..=end_rel].to_string()
}

/// Does `grammar` mention `module` as a whole identifier (flanked by
/// non-word characters on both sides)? This avoids false positives
/// like `int` matching `internal` or `time` matching `timeout`, while
/// remaining agnostic to the specific regex-alternation punctuation
/// used by each grammar (vim uses `\|`, TextMate uses `|`).
fn grammar_mentions_module(grammar: &str, module: &str) -> bool {
    let bytes = grammar.as_bytes();
    let mlen = module.len();
    let mbytes = module.as_bytes();
    if bytes.len() < mlen {
        return false;
    }
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i + mlen <= bytes.len() {
        if &bytes[i..i + mlen] == mbytes {
            let left_ok = i == 0 || !is_word(bytes[i - 1]);
            let right_ok = i + mlen == bytes.len() || !is_word(bytes[i + mlen]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[test]
fn every_builtin_module_appears_in_both_editor_grammars() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    // Narrow to the region that actually defines the module-name
    // alternation, so unrelated comment mentions of e.g. `set` or
    // `map` elsewhere in the grammar cannot mask a removal.
    let vim_scope = vim_module_line(&vim_raw);
    let vscode_scope = vscode_modules_block(&vscode_raw);

    let mut missing: Vec<String> = Vec::new();

    for &module in BUILTIN_MODULES {
        if !grammar_mentions_module(vim_scope, module) {
            missing.push(format!(
                "editors/vim/syntax/silt.vim (siltModule alternation) is missing builtin module \
                 `{}`",
                module
            ));
        }
        if !grammar_mentions_module(&vscode_scope, module) {
            missing.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json (\"modules\" entry) is missing \
                 builtin module `{}`",
                module
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "Editor syntax grammars are out of sync with src/module.rs BUILTIN_MODULES.\n\
         Add the following module name(s) to the grammar file(s) listed:\n  - {}\n\
         Authoritative source: src/module.rs (BUILTIN_MODULES).",
        missing.join("\n  - ")
    );
}
