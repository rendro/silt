//! Regression lock: the string-escape character classes advertised by
//! both editor grammars must equal the exact escape set the lexer
//! accepts inside double-quoted strings.
//!
//! The lexer (src/lexer.rs, string-escape match arms) accepts exactly:
//!     \n \t \\ \" \{ \}
//! and hard-errors "unknown escape sequence: \<c>" on anything else —
//! notably `\r` is REJECTED.
//!
//! Before this lock (round 101):
//!   - editors/vim/syntax/silt.vim `siltStringEscape` was `\\[nrt\\"]`
//!     — advertised the invalid `\r` and missed the valid `\{` / `\}`
//!     (so `"\{x}"` mis-highlighted as interpolation because the
//!     escape match never consumed the brace).
//!   - editors/vscode/syntaxes/silt.tmLanguage.json's
//!     `constant.character.escape.silt` was `\\[nrt\\"{}]` — also
//!     advertised the invalid `\r`.
//! A file containing `"a\rb"` highlighted as a valid escape in both
//! editors but fails `silt check`.
//!
//! This lock is derivation-based on both sides: the grammar classes
//! are decoded from the grammar files, and the lexer's accepted set is
//! discovered by DRIVING the real lexer over every printable-ASCII
//! candidate escape. It therefore goes red if either grammar's class
//! gains/loses a character, OR if the lexer's escape set itself
//! changes without the grammars following.
//!
//! Sibling locks in this family:
//!   - tests/meta/editor_grammar_keywords_tests.rs
//!   - tests/meta/editor_grammar_primitives_tests.rs
//!   - tests/meta/editor_grammar_constructors_tests.rs
//!   - tests/meta/editor_grammar_builtins_tests.rs
//!   - tests/meta/editor_grammar_modules_tests.rs
//! (the round-86 operator parity lock covers operators; this file adds
//! the previously missing escape-class coverage).

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_grammar(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// Does the real lexer accept `\<c>` as a string escape? Drives
/// `Lexer::tokenize` on the two-character string literal `"\<c>"`.
/// Any rejected escape makes the whole tokenize fail with either
/// "unknown escape sequence" or a downstream unterminated-string
/// error, so `is_ok()` is exactly "this escape is valid".
fn lexer_accepts_escape(c: char) -> bool {
    let src = format!("\"\\{c}\"");
    silt::lexer::Lexer::new(&src).tokenize().is_ok()
}

/// The lexer's accepted escape set, discovered empirically over all
/// printable-ASCII candidates plus any extra characters that appear in
/// a grammar class (so a grammar advertising a non-printable or
/// non-ASCII escape still gets probed and caught).
fn lexer_escape_set(extra: &HashSet<char>) -> HashSet<char> {
    let mut out = HashSet::new();
    for b in 0x20u8..=0x7e {
        let c = b as char;
        if lexer_accepts_escape(c) {
            out.insert(c);
        }
    }
    for &c in extra {
        if lexer_accepts_escape(c) {
            out.insert(c);
        }
    }
    out
}

/// Decodes a regex-style bracket character class from a pattern of the
/// form `\\[<class>]` (as it appears both in the vim pattern and in the
/// JSON-decoded VS Code regex): finds the `\\[` opener, then reads the
/// class body up to the closing `]`, resolving `\<c>` pairs to the
/// literal `<c>`. Panics on `-` because neither grammar may silently
/// introduce a character RANGE — this lock only understands literal
/// members, and a range would defeat the set-equality check.
fn parse_bracket_class(pattern: &str, which: &str) -> HashSet<char> {
    let open = pattern.find("\\\\[").unwrap_or_else(|| {
        panic!(
            "{which} string-escape pattern {pattern:?} does not contain the \
             expected `\\\\[` (escaped-backslash + class open) anchor"
        )
    });
    let body = &pattern[open + 3..];
    let mut out = HashSet::new();
    let mut chars = body.chars();
    loop {
        match chars.next() {
            None => panic!("{which} string-escape class in {pattern:?} has no closing `]`"),
            Some(']') => break,
            Some('\\') => {
                let escaped = chars.next().unwrap_or_else(|| {
                    panic!("{which} string-escape class in {pattern:?} ends mid-escape")
                });
                out.insert(escaped);
            }
            Some('-') => panic!(
                "{which} string-escape class in {pattern:?} contains `-`: character \
                 ranges are not supported by this parity lock — list members literally"
            ),
            Some(c) => {
                out.insert(c);
            }
        }
    }
    assert!(
        !out.is_empty(),
        "{which} string-escape class in {pattern:?} decoded to an empty set"
    );
    out
}

/// Extracts the vim `siltStringEscape` pattern (the single-quoted
/// argument of its `syntax match` line) and decodes its bracket class.
fn vim_escape_class(vim: &str) -> HashSet<char> {
    let line = vim
        .lines()
        .find(|l| l.contains("syntax match") && l.contains("siltStringEscape"))
        .expect(
            "editors/vim/syntax/silt.vim must contain a \
             `syntax match siltStringEscape '...' contained` line — \
             this regression-lock test needs it",
        );
    let first = line
        .find('\'')
        .unwrap_or_else(|| panic!("vim siltStringEscape line {line:?} has no opening quote"));
    let rest = &line[first + 1..];
    let second = rest
        .find('\'')
        .unwrap_or_else(|| panic!("vim siltStringEscape line {line:?} has no closing quote"));
    parse_bracket_class(&rest[..second], "vim")
}

/// Recursively searches the parsed tmLanguage JSON for the object
/// named `constant.character.escape.silt` and returns its `match`
/// regex (JSON escapes already decoded by serde_json).
fn find_vscode_escape_match(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if map.get("name").and_then(|n| n.as_str()) == Some("constant.character.escape.silt") {
                return map
                    .get("match")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string());
            }
            map.values().find_map(find_vscode_escape_match)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_vscode_escape_match),
        _ => None,
    }
}

fn vscode_escape_class(vscode_raw: &str) -> HashSet<char> {
    let json: serde_json::Value = serde_json::from_str(vscode_raw)
        .expect("editors/vscode/syntaxes/silt.tmLanguage.json is not valid JSON");
    let pattern = find_vscode_escape_match(&json).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain a \
         `constant.character.escape.silt` pattern with a `match` regex — \
         this regression-lock test needs it",
    );
    parse_bracket_class(&pattern, "vscode")
}

fn fmt_set(set: &HashSet<char>) -> String {
    let mut v: Vec<char> = set.iter().copied().collect();
    v.sort_unstable();
    v.iter()
        .map(|c| format!("`\\{c}`"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Baseline: lock the lexer's own escape set so the two grammar
/// parity checks below are anchored to a known-audited set. If the
/// lexer legitimately gains/loses an escape, update this list AND both
/// editor grammars together.
#[test]
fn lexer_accepted_escape_set_baseline() {
    let expected: HashSet<char> = ['n', 't', '\\', '"', '{', '}'].into_iter().collect();
    let actual = lexer_escape_set(&HashSet::new());
    assert_eq!(
        actual,
        expected,
        "the lexer's accepted string-escape set changed (now: {}). Update this \
         baseline, editors/vim/syntax/silt.vim (siltStringEscape) and \
         editors/vscode/syntaxes/silt.tmLanguage.json \
         (constant.character.escape.silt) together.",
        fmt_set(&actual)
    );
    // Spot-check the historical drift char: `\r` must stay rejected
    // unless the lexer deliberately adds it.
    assert!(
        !lexer_accepts_escape('r'),
        "lexer now accepts `\\r` — update both editor grammars and this baseline"
    );
}

#[test]
fn vim_string_escape_class_matches_lexer() {
    let vim_raw = read_grammar("editors/vim/syntax/silt.vim");
    let vim_set = vim_escape_class(&vim_raw);
    let lexer_set = lexer_escape_set(&vim_set);
    assert_eq!(
        vim_set,
        lexer_set,
        "editors/vim/syntax/silt.vim siltStringEscape class ({}) drifted from \
         the lexer's accepted escape set ({}). A char advertised but rejected \
         highlights as valid yet fails `silt check`; a char accepted but \
         missing mis-highlights (an absent `\\{{` lets the interpolation \
         region fire on the escaped brace).",
        fmt_set(&vim_set),
        fmt_set(&lexer_set)
    );
}

#[test]
fn vscode_string_escape_class_matches_lexer() {
    let vscode_raw = read_grammar("editors/vscode/syntaxes/silt.tmLanguage.json");
    let vscode_set = vscode_escape_class(&vscode_raw);
    let lexer_set = lexer_escape_set(&vscode_set);
    assert_eq!(
        vscode_set,
        lexer_set,
        "editors/vscode/syntaxes/silt.tmLanguage.json \
         constant.character.escape.silt class ({}) drifted from the lexer's \
         accepted escape set ({}). A char advertised but rejected highlights \
         as valid yet fails `silt check`; a char accepted but missing \
         mis-highlights.",
        fmt_set(&vscode_set),
        fmt_set(&lexer_set)
    );
}
