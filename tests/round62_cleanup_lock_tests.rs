//! Round 62 audit-cleanup regression locks.
//!
//! Round 62 swept four maintenance findings flagged by the audit:
//!
//! - L4: 27 stale `#[allow(dead_code)]` annotations on live `*_MD`
//!   constants and helper fns in `src/typechecker/builtins/docs.rs`.
//! - L5: two unused `pub fn`s (`clear_aliases`,
//!   `clear_assoc_bindings`) in `src/types/canonical.rs`.
//! - L6: weak-OR `joined.contains("arity")` substring branch in
//!   `tests/record_arity_tests.rs` whose `"arity"` token does not
//!   appear in the canonical post-round-60 diagnostic.
//! - L7: vim grammar covered fewer operators than vscode; round 62
//!   extended vim and locked the operator-set parity here.
//!
//! Each test below is a single-purpose source-text lock against the
//! corresponding finding so future drift surfaces immediately.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// L4: every `#[allow(dead_code)]` annotation in
/// `src/typechecker/builtins/docs.rs` is stale — every annotated
/// symbol has at least one in-tree caller under default features
/// (verified by grep at the time of the round 62 sweep). Lock the
/// file against any reintroduction of the annotation.
#[test]
fn typechecker_builtins_docs_has_no_stale_allow_dead_code() {
    let src = read_repo_file("src/typechecker/builtins/docs.rs");
    assert!(
        !src.contains("#[allow(dead_code)]"),
        "src/typechecker/builtins/docs.rs must not contain any \
         `#[allow(dead_code)]` annotations — every `*_MD` constant and \
         every `attach_module_*` helper has live callers under default \
         features. If a new dead annotation creeps in, either the \
         caller was removed (in which case delete the symbol too) or \
         the annotation is a stale carry-over from an earlier phase \
         (in which case delete the annotation)."
    );
}

/// L5: `clear_aliases` and `clear_assoc_bindings` had zero callers
/// across the entire repo; their docstrings advertised them as test-
/// isolation hooks but no test actually used them. Round 62 deleted
/// both functions and rewrote the cross-reference comments to
/// document the absence honestly. Lock the source against any silent
/// reintroduction of either function.
#[test]
fn canonical_does_not_export_unused_clear_helpers() {
    let src = read_repo_file("src/types/canonical.rs");
    assert!(
        !src.contains("pub fn clear_aliases"),
        "src/types/canonical.rs must not export `clear_aliases` — the \
         alias registry is intentionally process-global and not cleared \
         between checks. If a real caller appears (e.g. an out-of- \
         process integration harness), reintroduce the function with a \
         docstring that names the caller and remove this lock."
    );
    assert!(
        !src.contains("pub fn clear_assoc_bindings"),
        "src/types/canonical.rs must not export `clear_assoc_bindings` \
         — same rationale as `clear_aliases`. Reintroduce only when a \
         real caller exists."
    );
}

/// L6: the canonical post-round-60 arity diagnostic is
/// `"type argument count mismatch for <name>: expected 0, got <n>"`.
/// The bare `joined.contains("arity")` substring branch deleted in
/// round 62 was a future-drift escape hatch — the canonical phrase
/// does NOT contain the word "arity", so the branch was dead AND
/// would silently mask a rephrasing that dropped both anchored
/// phrases. Lock the test source against a reintroduction of the
/// weak-OR branch.
#[test]
fn record_arity_test_does_not_use_weak_or_substring() {
    let src = read_repo_file("tests/record_arity_tests.rs");
    assert!(
        !src.contains("joined.contains(\"arity\")"),
        "tests/record_arity_tests.rs must not weak-OR on the bare \
         substring `\"arity\"` — the canonical post-round-60 \
         diagnostic does not contain that token. Anchor the assertion \
         on the canonical phrase (`expected 0` and `got`) instead."
    );
}

/// L7: extract the operator-literal set covered by each editor
/// grammar and assert the vim grammar covers AT LEAST every literal
/// the vscode grammar covers. Round 62 extended vim with the
/// arithmetic/assignment/relational single-character operators that
/// vscode already covered; this lock prevents the next vscode
/// addition from drifting away from the vim grammar silently.
#[test]
fn vim_and_vscode_grammars_share_operator_set() {
    let vim = read_grammar_relative("editors/vim/syntax/silt.vim");
    let vscode = read_grammar_relative("editors/vscode/syntaxes/silt.tmLanguage.json");

    let vim_ops = vim_operator_literals(&vim);
    let vscode_ops = vscode_operator_literals(&vscode);

    // Sanity: both extractors must find the round-62 known-good set,
    // extended in round 86 with the `...` (DotDotDot, record spread/
    // rest) and `::` (ColonColon, assoc-type projection) operators
    // that the lexer has long emitted but the grammars had silently
    // omitted. If either of these baseline asserts fails, the
    // extractor is broken — fix the extractor before debugging
    // grammar drift.
    let baseline: BTreeSet<&'static str> = [
        "|>", "->", "...", "..", "::", "?", "^", "&&", "||", "==", "!=", "<=", ">=", "!", "+", "-",
        "*", "/", "%", "=", "<", ">",
    ]
    .into_iter()
    .collect();
    let baseline_strings: BTreeSet<String> = baseline.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        vim_ops, baseline_strings,
        "vim operator-literal extractor diverged from the round-62 baseline.\n\
         expected: {:?}\nfound:    {:?}",
        baseline_strings, vim_ops
    );
    assert_eq!(
        vscode_ops, baseline_strings,
        "vscode operator-literal extractor diverged from the round-62 baseline.\n\
         expected: {:?}\nfound:    {:?}",
        baseline_strings, vscode_ops
    );

    // Set equality between the two grammars is the actual lock.
    assert_eq!(
        vim_ops,
        vscode_ops,
        "vim and vscode grammars cover different operator sets.\n\
         vim only:    {:?}\nvscode only: {:?}",
        vim_ops.difference(&vscode_ops).collect::<Vec<_>>(),
        vscode_ops.difference(&vim_ops).collect::<Vec<_>>()
    );
}

fn read_grammar_relative(rel: &str) -> String {
    read_repo_file(rel)
}

/// Extract the literal operator strings declared by the vim grammar's
/// `siltOperator` matches. Each line has the shape
/// `syntax match siltOperator "<pattern>"`; we strip vim's `\` escape
/// (used to escape regex metachars like `*`, `^`, `.`, `?` so they
/// match literally in vim's `nomagic`-ish syntax mode) so the result
/// is the set of literal operator tokens the grammar highlights.
fn vim_operator_literals(vim: &str) -> BTreeSet<String> {
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

/// Strip vim regex escapes from a literal-operator pattern. Vim uses
/// `\` to mark a metachar as literal (e.g. `\.\.` means two literal
/// dots, `\*` means a literal star). The grammar's operator patterns
/// are pure literals (no alternations, no quantifiers), so blindly
/// dropping every `\` recovers the operator string.
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

/// Extract the literal operator strings declared by the vscode
/// grammar's `operators` repository entry. The entry contains a
/// `patterns` array whose items each have a `match` regex; we walk
/// the regex tokens, decode `\\X` escapes, and split character-class
/// shorthands (`[+\-*/%]`, `[<>]`) and pipe-alternation (`==|!=|<=|>=`,
/// `&&|\|\||!`) into individual operator literals.
fn vscode_operator_literals(vscode: &str) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let block = extract_vscode_operators_block(vscode);
    for raw in extract_vscode_match_regexes(&block) {
        for op in split_vscode_match_regex(&raw) {
            out.insert(op);
        }
    }
    out
}

/// Return the substring of `vscode` from `"operators"` through the
/// matching `]` that closes the `patterns` array. The patterns array
/// has no nested `[`, so a depth-1 scan after the first `[` after
/// `"patterns"` is sufficient.
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

/// Pull every JSON `"match": "<regex>"` value out of the supplied
/// vscode operators block. The values are single-line strings whose
/// only escape relevant here is `\\` for a literal backslash and
/// `\"` (which does not appear in any of silt's operator regexes).
fn extract_vscode_match_regexes(block: &str) -> Vec<String> {
    let mut out = Vec::new();
    let needle = "\"match\"";
    let mut i = 0usize;
    while let Some(rel) = block[i..].find(needle) {
        let abs = i + rel;
        let after = &block[abs + needle.len()..];
        // Skip past `:` and whitespace to the opening `"`.
        let Some(open_rel) = after.find('"') else {
            break;
        };
        let value_start = abs + needle.len() + open_rel + 1;
        // Find the next unescaped `"`.
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

/// Decompose a vscode-grammar operator regex into the set of literal
/// operator tokens it covers. Handles the four shapes used by the
/// silt grammar's `operators` entry:
///
/// - simple literal with backslash-escaped metachars (`\\|>`, `->`,
///   `\\.\\.`, `\\?`, `\\^`)
/// - `==|!=|<=|>=` — pipe-alternation of plain literals
/// - `&&|\\|\\||!` — pipe-alternation that itself contains
///   backslash-escaped pipes
/// - `[+\\-*/%]` and `[<>]` — single-char class
fn split_vscode_match_regex(re: &str) -> Vec<String> {
    // First, JSON-decode the `\\` escape: in the JSON file it is two
    // chars `\` `\` representing a single regex backslash.
    let regex = decode_json_escapes(re);
    if regex.starts_with('[') && regex.ends_with(']') {
        // Character class: split each member, decoding the regex `\X`
        // escape (e.g. `\-` means literal `-`).
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
    // Pipe-alternation at the top level. The `&&|\|\||!` case has
    // backslash-escaped pipes that must NOT be treated as separators.
    let mut alternatives: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = regex.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Consume the next char as a literal.
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

/// Decode `\\` -> `\` (the only JSON string escape that appears in
/// silt's operator regexes). Leaves other escapes alone — none are
/// used.
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
