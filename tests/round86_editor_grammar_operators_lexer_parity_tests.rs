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
//! `tests/round62_cleanup_lock_tests.rs::vim_and_vscode_grammars_share_operator_set`
//! was symmetric drift-blind: it locked the two grammars to a fixed
//! 20-item baseline that already omitted both tokens, so adding a new
//! token to the lexer without updating the grammars would slip
//! through unflagged. Round 86 extended that baseline AND added this
//! lexer-anchored test so the canonical operator list lives next to
//! the lexer rather than next to the grammars.
//!
//! ## What this test locks
//!
//! For every multi-character operator the lexer emits via
//! `impl Display for Token`, assert:
//!
//! 1. The `write!(f, "<op>")` line exists in `src/lexer.rs` —
//!    anchoring the test's operator list to the source of truth.
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

/// The canonical set of multi-character operators the lexer can
/// emit. This list MUST match the multi-char `write!(f, "{op}")`
/// arms in `impl fmt::Display for Token` in `src/lexer.rs`. Each
/// entry is verified against the lexer source below, so a drift
/// here surfaces as a test failure rather than as a silent grammar
/// gap.
///
/// Single-char operators (`+`, `-`, `*`, `/`, `%`, `=`, `<`, `>`,
/// `!`, `?`, `^`, `|`, `:`, `,`, `.`, `(`, `)`, `[`, `]`, `{`, `}`)
/// are intentionally excluded — round 62's lock at
/// `tests/round62_cleanup_lock_tests.rs` already covers the
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

/// Anchor: every entry in `MULTI_CHAR_OPERATORS` corresponds to a
/// `write!(f, "<op>")` arm in `impl fmt::Display for Token`. We
/// `include_str!` the lexer source directly so this test fails the
/// moment a new multi-char Display arm appears without a matching
/// entry here (and grammar coverage assertions below).
#[test]
fn multi_char_operator_list_matches_lexer_display_impl() {
    let lexer = include_str!("../src/lexer.rs");
    for op in MULTI_CHAR_OPERATORS {
        let needle = format!("write!(f, \"{op}\")");
        assert!(
            lexer.contains(&needle),
            "operator `{op}` listed in MULTI_CHAR_OPERATORS but no \
             `{needle}` arm found in src/lexer.rs::impl Display for Token. \
             Either the operator was removed from the lexer (drop it \
             from MULTI_CHAR_OPERATORS) or its Display formatting was \
             changed (update the needle here)."
        );
    }

    // Reverse direction: enumerate every multi-char `write!(f, "...")`
    // arm in the Display impl and assert each one is in our list.
    // This catches the "new lexer token, forgot to add to grammar
    // parity list" case. We scan the Display impl block specifically
    // by anchoring on the function signature; we skip arms whose
    // payload is a single char (those are not multi-char operators
    // and are excluded by design above), plus the well-known
    // non-operator Display payloads (keywords, EOF, newline, literal
    // placeholders, collection prefixes).
    let display_block = extract_display_impl_block(lexer);
    let mut found: BTreeSet<String> = BTreeSet::new();
    for line in display_block.lines() {
        let trimmed = line.trim();
        // Only consider arms whose payload is a plain literal string
        // (no format placeholders `{...}`).
        let Some(op) = extract_plain_write_payload(trimmed) else {
            continue;
        };
        // Skip single-char tokens (handled by round-62 lock or
        // not grammar-highlighted at all).
        if op.chars().count() < 2 {
            continue;
        }
        // Skip the keyword arms — keywords are highlighted via
        // `syntax keyword siltKeyword` in vim and `keywords` in
        // vscode, not as operators.
        if KEYWORD_LITERALS.contains(&op.as_str()) {
            continue;
        }
        // Skip Display payloads that are not operators:
        // `EOF`, `\n`, collection prefixes (`#{`, `#[`).
        if op == "EOF" || op == "\\n" || op == "#{" || op == "#[" {
            continue;
        }
        found.insert(op);
    }
    let expected: BTreeSet<String> = MULTI_CHAR_OPERATORS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        found,
        expected,
        "Display impl multi-char operator arms drifted from \
         MULTI_CHAR_OPERATORS.\nin Display but not listed: {:?}\n\
         listed but not in Display: {:?}",
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

/// Keywords whose `write!(f, "<word>")` arms in the lexer Display
/// impl are NOT operators. These are excluded from the reverse
/// drift check in `multi_char_operator_list_matches_lexer_display_impl`.
const KEYWORD_LITERALS: &[&str] = &[
    "let", "fn", "type", "trait", "match", "when", "return", "pub", "mod", "import", "as", "else",
    "where", "loop",
];

/// Return the substring of the lexer source covering the body of
/// `impl fmt::Display for Token { fn fmt(&self, f: ...) -> ... { ... } }`.
/// We find the signature line, then return everything up to the
/// matching closing `}` of the function body.
fn extract_display_impl_block(lexer: &str) -> String {
    let needle = "impl fmt::Display for Token";
    let start = lexer
        .find(needle)
        .expect("src/lexer.rs must contain `impl fmt::Display for Token`");
    // Find the opening `{` of the impl block, then of the inner
    // `fn fmt`, then walk to the matching close of that fn body.
    let tail = &lexer[start..];
    let impl_open = tail
        .find('{')
        .expect("impl Display for Token missing opening `{`");
    let after_impl_open = &tail[impl_open + 1..];
    let fn_open_rel = after_impl_open
        .find('{')
        .expect("fn fmt body opening `{` not found");
    let body_start = impl_open + 1 + fn_open_rel + 1;
    let bytes = tail.as_bytes();
    let mut depth = 1i32;
    let mut i = body_start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return tail[body_start..i].to_string();
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("could not find closing `}}` of fn fmt body in lexer source");
}

/// Extract the literal payload of a `write!(f, "...")` call whose
/// argument is a plain string (no format placeholders, no escapes
/// beyond the universal `\\` and `\"`). Returns `None` for any
/// other shape (e.g. `write!(f, "{n}")`, `write!(f, "{s}{{")` for
/// the interpolation segments — those are not plain-literal
/// operators).
fn extract_plain_write_payload(line: &str) -> Option<String> {
    let prefix = "write!(f, \"";
    let p = line.find(prefix)?;
    let after = &line[p + prefix.len()..];
    // Find the closing `"`. The lexer Display arms only use `\"` as
    // escape inside string-literal arms (`"\"{s}\""`); for those
    // arms the unescaped content still contains `{`, so we'll filter
    // them out below by rejecting any payload with `{` or `}`.
    let mut bytes = after.bytes();
    let mut end = None;
    let mut prev: u8 = 0;
    let mut idx = 0;
    for b in bytes.by_ref() {
        if b == b'"' && prev != b'\\' {
            end = Some(idx);
            break;
        }
        prev = b;
        idx += 1;
    }
    let end = end?;
    let payload = &after[..end];
    // Reject payloads with format placeholders (`{` or `}`), which
    // signal non-plain-literal arms (numeric/string/interpolation
    // segments). Note: the `LBrace`/`RBrace` arms write `{{`/`}}`,
    // which the format machinery emits as a single `{`/`}` —
    // those are single-char operators and would be filtered by the
    // length check at the call site anyway. But for safety reject
    // any `{` or `}` here.
    if payload.contains('{') || payload.contains('}') {
        return None;
    }
    // Decode the only relevant escape: `\\` -> `\`. Operator
    // literals don't contain backslashes, but the `Newline` arm
    // writes `\n` as `\\n` source — handle generically.
    let mut out = String::with_capacity(payload.len());
    let mut chars = payload.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

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
