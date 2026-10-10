//! Round 86: lexer-anchored editor-grammar operator parity lock.
//!
//! ## Bug fixed (G1)
//!
//! `src/lexer.rs` defines `DotDotDot` (`...`, record spread / rest)
//! and `ColonColon` (`::`, associated-type projection — `Self::Item`,
//! `<T as Trait>::Item`). Both editor grammars omitted them:
//!
//! - `editors/vim/syntax/silt.vim`
//! - `editors/vscode/syntaxes/silt.tmLanguage.json`
//!
//! The pre-existing parity lock at
//! `tests/meta/round62_cleanup_lock_tests.rs::vim_and_vscode_grammars_share_operator_set`
//! was symmetric drift-blind: it locked the two grammars to a fixed
//! 20-item baseline that already omitted both tokens, so adding a new
//! token to the lexer without updating the grammars would slip
//! through unflagged. Round 86 extended that baseline AND added this
//! lexer-anchored test so the canonical operator list lives next to
//! the lexer rather than next to the grammars.
//!
//! ## What this test locks
//!
//! For every multi-character operator the lexer emits, assert:
//!
//! 1. The operator list equals the multi-char operators the real lexer
//!    produces (discovered by lexing every short punctuation string),
//!    anchoring the list to the lexer's behaviour.
//! 2. The vim grammar (`editors/vim/syntax/silt.vim`) contains a
//!    `syntax match siltOperator` whose literal pattern, after
//!    stripping vim's `\` escapes, equals the operator.
//! 3. The vscode grammar
//!    (`editors/vscode/syntaxes/silt.tmLanguage.json`) contains an
//!    `operators` repository pattern whose literal `match` regex,
//!    after JSON-decoding and stripping regex escapes, covers the
//!    operator (either directly or as one alternative of a
//!    pipe-alternation).
//!
//! Adding a new multi-char operator to the lexer therefore requires
//! adding it to this test's list AND to both grammar files in the
//! same change.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use silt::lexer::{Lexer, Token};

/// The canonical set of multi-character operators the lexer can
/// emit, checked against the real lexer by
/// `multi_char_operator_list_matches_lexer` below.
///
/// Single-char operators (`+`, `-`, `*`, `/`, `%`, `=`, `<`, `>`,
/// `!`, `?`, `^`, `|`, `:`, `,`, `.`, `(`, `)`, `[`, `]`, `{`, `}`)
/// are intentionally excluded — round 62's lock at
/// `tests/meta/round62_cleanup_lock_tests.rs` already covers the
/// subset of those that participate in `siltOperator` matches, and
/// the delimiters are not grammar-highlighted as operators.
///
/// `#{` and `#[` are collection prefixes, not operators, and are
/// covered by `collection-prefix` / `siltCollectionPrefix` in the
/// grammars; they are excluded here.
const MULTI_CHAR_OPERATORS: &[&str] = &[
    "==",  // EqEq
    "!=",  // NotEq
    "<=",  // LtEq
    ">=",  // GtEq
    "&&",  // AndAnd
    "||",  // OrOr
    "|>",  // Pipe
    "..",  // DotDot
    "...", // DotDotDot — round 86 added
    "->",  // Arrow
    "::",  // ColonColon — round 86 added
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// The lexer's multi-character operators, discovered by DRIVING the
/// real lexer over every 2- and 3-character string of punctuation: a
/// candidate is an operator when it lexes to exactly one token whose
/// `Display` is the candidate itself. The collection prefixes `#{` and
/// `#[` are excluded (they are not operators).
fn lexer_multi_char_operators() -> BTreeSet<String> {
    const PUNCT: &[char] = &[
        '=', '!', '<', '>', '&', '|', '.', '-', ':', '+', '*', '/', '%', '?', '^', '#', '@', '~',
        '$', ';', ',', '(', ')', '[', ']', '{', '}',
    ];
    let mut candidates: Vec<String> = Vec::new();
    for &a in PUNCT {
        for &b in PUNCT {
            candidates.push(format!("{a}{b}"));
            for &c in PUNCT {
                candidates.push(format!("{a}{b}{c}"));
            }
        }
    }
    let mut found = BTreeSet::new();
    for cand in candidates {
        if cand == "#{" || cand == "#[" {
            continue;
        }
        let Ok(tokens) = Lexer::new(silt::source::FileId::default(), &cand)
            .tokenize()
            .checked()
        else {
            continue;
        };
        let significant: Vec<Token> = tokens
            .tokens
            .into_iter()
            .map(|tok| tok.kind)
            .filter(|tok| !matches!(tok, Token::Eof))
            .collect();
        if let [tok] = significant.as_slice()
            && tok.to_string() == cand
        {
            found.insert(cand);
        }
    }
    found
}

/// Anchor: `MULTI_CHAR_OPERATORS` is exactly the set of multi-character
/// operators the real lexer produces, in both directions — a new lexer
/// operator that is missing from the list (and so from the grammar
/// checks below) fails here.
#[test]
fn multi_char_operator_list_matches_lexer() {
    let found = lexer_multi_char_operators();
    let expected: BTreeSet<String> = MULTI_CHAR_OPERATORS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        found,
        expected,
        "the lexer's multi-char operators drifted from \
         MULTI_CHAR_OPERATORS.\nlexed but not listed: {:?}\n\
         listed but not lexed: {:?}",
        found.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&found).collect::<Vec<_>>()
    );
}

/// Vim grammar must contain a `syntax match siltOperator "<pattern>"`
/// for every multi-char operator. Vim escapes regex metachars with
/// `\` (e.g. `\.\.` for two literal dots); we strip those before
/// comparing.
#[test]
fn vim_grammar_covers_every_multi_char_lexer_operator() {
    let vim = read_repo_file("editors/vim/syntax/silt.vim");
    let vim_ops = extract_vim_operator_literals(&vim);
    for op in MULTI_CHAR_OPERATORS {
        assert!(
            vim_ops.contains(*op),
            "vim grammar `editors/vim/syntax/silt.vim` is missing a \
             `syntax match siltOperator` for the lexer operator `{op}`. \
             Found operators: {:?}\n\
             Add a line like `syntax match siltOperator \"<escaped-pattern>\"` \
             — see existing entries for the escape convention. \
             Multi-char operators should appear in longest-first order \
             so vim's longest-match rule prefers them (e.g. `...` before \
             `..`).",
            vim_ops
        );
    }
}

/// VSCode grammar must contain an `operators` pattern that matches
/// every multi-char operator. The patterns are JSON `"match"` regex
/// strings; we decode the JSON `\\` -> `\` escape, then the regex
/// `\X` -> `X` escape, then split top-level pipe-alternation into
/// individual literals.
#[test]
fn vscode_grammar_covers_every_multi_char_lexer_operator() {
    let vscode = read_repo_file("editors/vscode/syntaxes/silt.tmLanguage.json");
    let vscode_ops = extract_vscode_operator_literals(&vscode);
    for op in MULTI_CHAR_OPERATORS {
        assert!(
            vscode_ops.contains(*op),
            "vscode grammar `editors/vscode/syntaxes/silt.tmLanguage.json` \
             is missing an `operators` pattern for the lexer operator \
             `{op}`. Found operators: {:?}\n\
             Add a pattern like `{{ \"name\": \"keyword.operator.<kind>.silt\", \
             \"match\": \"<escaped-regex>\" }}` to the `operators.patterns` \
             array — see existing entries for the escape convention. \
             Multi-char operators that share a prefix with another must \
             come first in the array (e.g. `...` before `..`).",
            vscode_ops
        );
    }
}

// ── Helpers ────────────────────────────────────────────────────────

/// Pull every literal-operator pattern out of the vim grammar file.
fn extract_vim_operator_literals(vim: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for line in vim.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("syntax match siltOperator") {
            continue;
        }
        let Some(open) = trimmed.find('"') else {
            continue;
        };
        let after = &trimmed[open + 1..];
        let Some(close) = after.find('"') else {
            continue;
        };
        let raw = &after[..close];
        out.insert(unescape_vim_pattern(raw));
    }
    out
}

fn unescape_vim_pattern(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Pull every literal-operator pattern out of the vscode grammar's
/// `operators` repository entry. Mirrors the extractor in round 62
/// but kept local so this test stands alone (no inter-test coupling).
fn extract_vscode_operator_literals(vscode: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let block = extract_vscode_operators_block(vscode);
    for raw in extract_vscode_match_regexes(&block) {
        for op in split_vscode_match_regex(&raw) {
            out.insert(op);
        }
    }
    out
}

fn extract_vscode_operators_block(vscode: &str) -> String {
    let key = "\"operators\"";
    let start = vscode.find(key).expect(
        "editors/vscode/syntaxes/silt.tmLanguage.json must contain an \
         \"operators\" repository entry",
    );
    let tail = &vscode[start..];
    let patterns_at = tail
        .find("\"patterns\"")
        .expect("\"operators\" entry must contain a \"patterns\" array");
    let array_open = tail[patterns_at..]
        .find('[')
        .expect("\"patterns\" array opening `[` not found");
    let abs_open = patterns_at + array_open;
    let mut depth = 0i32;
    let bytes = tail.as_bytes();
    let mut i = abs_open;
    let mut close_at: Option<usize> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close_at = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let close = close_at.expect("\"patterns\" array closing `]` not found");
    tail[..=close].to_string()
}

fn extract_vscode_match_regexes(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let needle = "\"match\"";
    let mut i = 0usize;
    while let Some(rel) = block[i..].find(needle) {
        let abs = i + rel;
        let after = &block[abs + needle.len()..];
        let Some(open_rel) = after.find('"') else {
            break;
        };
        let value_start = abs + needle.len() + open_rel + 1;
        let bytes = block.as_bytes();
        let mut j = value_start;
        while j < bytes.len() {
            if bytes[j] == b'"' && bytes[j - 1] != b'\\' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        out.push(block[value_start..j].to_string());
        i = j + 1;
    }
    out
}

fn split_vscode_match_regex(re: &str) -> Vec<String> {
    let regex = decode_json_escapes(re);
    if regex.starts_with('[') && regex.ends_with(']') {
        let inner = &regex[1..regex.len() - 1];
        let mut out = Vec::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next.to_string());
                }
            } else {
                out.push(c.to_string());
            }
        }
        return out;
    }
    let mut alternatives: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = regex.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
        } else if c == '|' {
            alternatives.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    alternatives.push(current);
    alternatives.into_iter().filter(|s| !s.is_empty()).collect()
}

fn decode_json_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '\\' {
                    out.push('\\');
                    chars.next();
                    continue;
                }
            }
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}
