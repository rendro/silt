//! Regression lock: every primitive type name printed by
//! `src/types.rs`'s `impl Display for Type` must appear in both editor
//! syntax-highlighting grammars. Without this lock a user typing
//! `let x: Int = 1` in vim or VS Code sees `Int` rendered as a plain
//! identifier — no distinction between "this is a type" and "this is a
//! variable".
//!
//! Round-60 GAP G5 lock: the 9 primitive type names
//! (Int, Float, ExtFloat, Bool, String, Unit, List, Map, Set) were
//! absent from both grammars until this test was introduced. The
//! authoritative source is `src/types.rs` — adding a new primitive
//! there without updating both grammars would regress editor
//! highlighting silently; this test enforces the coupling.
//!
//! Round-61 extension: the set was widened to include the 6 builtin
//! container / callable / resource types recognised by
//! `src/typechecker/mod.rs::is_builtin_container` (Range, Channel,
//! Tuple, Fn, Fun, Handle). These are legal in type-annotation
//! position (see docs/language/operators.md, stdlib/channel-task.md,
//! stdlib/http.md, stdlib/stream.md) and must highlight as types.
//! Authoritative source of the widened list: the `match` arm in
//! `src/typechecker/mod.rs::is_builtin_container` (around line 2823).
//!
//! Mirrors the pattern of:
//!   - tests/editor_grammar_constructors_tests.rs
//!   - tests/editor_grammar_modules_tests.rs
//!
//! If this test fails after adding a new primitive to `src/types.rs`,
//! add the primitive name to:
//!   - editors/vim/syntax/silt.vim           (siltType keyword list)
//!   - editors/vscode/syntaxes/silt.tmLanguage.json ("primitives" match)

use std::fs;
use std::path::PathBuf;

/// Primitive and built-in container type names sourced from the
/// authoritative table at `silt::types::builtins::BUILTIN_TYPES`. The
/// `()` surface alias is filtered out — it is a punctuation form, not
/// a keyword token, and neither editor grammar represents it as such.
/// Every other entry in `BUILTIN_TYPES` is asserted present in both
/// grammars below.
///
/// Adding a new entry to `BUILTIN_TYPES` without also adding the name
/// to both editor grammars will fail this test.
fn primitives() -> Vec<&'static str> {
    silt::types::builtins::iter_all()
        .map(|b| b.name)
        .filter(|n| *n != "()")
        .collect()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Returns every line from the vim grammar that declares a `siltType`
/// keyword group, concatenated. Panics if no such line exists — that
/// is itself a regression.
fn vim_type_scope(vim: &str) -> String {
    let lines: Vec<&str> = vim
        .lines()
        .filter(|l| l.contains("siltType") && l.contains("syntax keyword"))
        .collect();
    assert!(
        !lines.is_empty(),
        "editors/vim/syntax/silt.vim must contain at least one \
         `syntax keyword siltType ...` line listing the primitive type \
         names — this regression-lock test needs it."
    );
    lines.join("\n")
}

/// Returns the portion of the VS Code grammar JSON that defines the
/// `primitives` repository entry (from the `"primitives"` key through
/// the closing brace of that object). A naive forward scan to the
/// first `}` is sufficient because the `"match"` value is a
/// single-line regex without nested braces.
fn vscode_primitives_block(vscode: &str) -> String {
    let marker = "\"primitives\"";
    let start = vscode.find(marker).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \"primitives\" \
         repository entry listing primitive type names — this regression-lock test needs it.",
    );
    let tail = &vscode[start..];
    let end_rel = tail
        .find('}')
        .expect("`\"primitives\"` entry is malformed: no closing brace found");
    tail[..=end_rel].to_string()
}

/// Does `grammar` mention `name` as a whole identifier (flanked by
/// non-word characters on both sides)? This avoids false positives
/// like `Int` matching `Integer` or `Map` matching `MapKey`, while
/// remaining agnostic to the specific regex-alternation punctuation
/// used by each grammar.
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
fn editor_grammars_include_all_primitive_type_names() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");

    // Narrow to the regions that actually define the type-name
    // alternation/keyword list, so unrelated mentions of e.g. `List`
    // elsewhere in the grammar (in a comment or the constructors
    // alternation) cannot mask a removal.
    let vim_scope = vim_type_scope(&vim_raw);
    let vscode_scope = vscode_primitives_block(&vscode_raw);

    let mut missing: Vec<String> = Vec::new();

    for prim in primitives() {
        if !grammar_mentions_name(&vim_scope, prim) {
            missing.push(format!(
                "editors/vim/syntax/silt.vim (siltType keyword list) is missing \
                 primitive type `{}`",
                prim
            ));
        }
        if !grammar_mentions_name(&vscode_scope, prim) {
            missing.push(format!(
                "editors/vscode/syntaxes/silt.tmLanguage.json (\"primitives\" entry) \
                 is missing primitive type `{}`",
                prim
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "Editor syntax grammars are out of sync with the primitive type \
         names printed by `src/types.rs::impl Display for Type`.\n\
         Add the following primitive name(s) to the grammar file(s) listed:\n  - {}\n\
         Authoritative source: src/types.rs (Type enum + Display impl).",
        missing.join("\n  - ")
    );
}
