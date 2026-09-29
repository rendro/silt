//! Number-literal parity lock between the lexer and the editor
//! grammars (vim + vscode).
//!
//! ## Bug fixed (GAP)
//!
//! `src/lexer.rs::scan_number` accepts hex (`0x`/`0X`) and binary
//! (`0b`/`0B`) integer literals, `_` digit separators, and `e`/`E`
//! exponents — and `docs/language/types.md` documents `0xFF`,
//! `0b1010`, and `1_000_000`. The editor grammars had drifted:
//!
//! - `editors/vim/syntax/silt.vim` had only `\<\d\+\>` and
//!   `\<\d\+\.\d\+\>` — no hex, no binary, no underscores, no
//!   exponents. `let mask = 0xFF` (a doc example) got no number
//!   highlight at all, and `1_000` highlighted only up to the `_`.
//! - `editors/vscode/syntaxes/silt.tmLanguage.json` hardcoded
//!   lowercase `0x`/`0b` prefixes, so the lexer-legal `0XFF`/`0B1`
//!   went unhighlighted, and its exponent digits (`\d+`) rejected
//!   the lexer-legal `1e1_0`.
//!
//! ## What this test locks
//!
//! A table of literal forms is anchored to the REAL lexer (each
//! `ACCEPTED` form must tokenize to exactly one `Int`/`Float` token
//! of the stated kind; each `REJECTED` form must not), then both
//! grammars' number patterns are extracted, decoded to plain regex,
//! and evaluated:
//!
//! 1. Every lexer-accepted form is fully matched by a pattern of the
//!    correct kind (float vs integer) in BOTH grammars.
//! 2. No grammar pattern fully matches a lexer-rejected form.
//! 3. Order locks: float patterns come after integer patterns in vim
//!    (last-defined wins at the same start column, so `1.5` must hit
//!    siltFloat, not siltNumber) and before the decimal-integer
//!    pattern in vscode (first match wins, so `1.5` must hit the
//!    float scope before `1` hits the integer scope).

use regex::Regex;
use silt::lexer::{Lexer, Token};
use std::fs;
use std::path::PathBuf;

/// Literal forms the lexer accepts as a single number token, with
/// the token kind it produces. Anchored to the real lexer in
/// `accepted_table_matches_lexer`.
const ACCEPTED: &[(&str, Kind)] = &[
    // Decimal integers, with `_` separators (lexer allows trailing `_`).
    ("0", Kind::Int),
    ("42", Kind::Int),
    ("1_000", Kind::Int),
    ("1_000_000", Kind::Int),
    ("1_", Kind::Int),
    // Hex — both prefix cases, mixed digit case, `_` separators
    // (including directly after the prefix).
    ("0xFF", Kind::Int),
    ("0XFF", Kind::Int),
    ("0xff", Kind::Int),
    ("0x1_F", Kind::Int),
    ("0x_F", Kind::Int),
    // Binary — both prefix cases, `_` separators.
    ("0b10", Kind::Int),
    ("0B10", Kind::Int),
    ("0b1010", Kind::Int),
    ("0b1_0", Kind::Int),
    // Floats with a fractional part, `_` separators in either half.
    ("1.5", Kind::Float),
    ("3.14", Kind::Float),
    ("1_000.5", Kind::Float),
    ("1.5_5", Kind::Float),
    // Exponents — with/without fraction, either case, optional sign,
    // `_` separators after the first exponent digit.
    ("1.5e-3", Kind::Float),
    ("1.5E+3", Kind::Float),
    ("1e5", Kind::Float),
    ("1E5", Kind::Float),
    ("2e+10", Kind::Float),
    ("9e-2", Kind::Float),
    ("1e1_0", Kind::Float),
];

/// Forms the lexer does NOT lex as a single number token (either a
/// lex error, or a non-number token like an identifier). No grammar
/// pattern may fully match any of these.
const REJECTED: &[&str] = &[
    "0x",   // no hex digit after prefix
    "0X",   //
    "0b",   // no binary digit after prefix
    "0B",   //
    "0xG",  // not a hex digit
    "0b2",  // not a binary digit
    "1e",   // exponent needs a digit
    "1e+",  // sign but no digit
    "1e-",  //
    "1.5e", //
    "1e_5", // exponent must START with a digit, not `_`
    "_1",   // identifier, not a number
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Int,
    Float,
}

// ── Lexer anchoring ────────────────────────────────────────────────

/// Lex `src`; return `Some(kind)` iff it produces exactly one
/// significant token and that token is a number literal.
fn lex_single_number(src: &str) -> Option<Kind> {
    let tokens = Lexer::new(src).tokenize().ok()?;
    let significant: Vec<Token> = tokens
        .into_iter()
        .map(|(tok, _span)| tok)
        .filter(|tok| !matches!(tok, Token::Newline | Token::Eof))
        .collect();
    match significant.as_slice() {
        [Token::Int(_)] => Some(Kind::Int),
        [Token::Float(_)] => Some(Kind::Float),
        _ => None,
    }
}

/// The tables above are only trustworthy if they agree with the real
/// lexer. Verify both directions before using them against the
/// grammars, so a lexer change surfaces here first.
#[test]
fn accepted_table_matches_lexer() {
    for (form, kind) in ACCEPTED {
        assert_eq!(
            lex_single_number(form),
            Some(*kind),
            "table drift: `{form}` should lex to a single {kind:?} \
             token per src/lexer.rs::scan_number — update ACCEPTED \
             (and both editor grammars) if the lexer changed"
        );
    }
}

#[test]
fn rejected_table_matches_lexer() {
    for form in REJECTED {
        assert_eq!(
            lex_single_number(form),
            None,
            "table drift: `{form}` unexpectedly lexes as a single \
             number token — move it to ACCEPTED and extend both \
             editor grammars to cover it"
        );
    }
}

// ── Vim grammar ────────────────────────────────────────────────────

#[test]
fn vim_grammar_covers_every_lexer_accepted_number_literal() {
    let patterns = vim_number_patterns();
    for (form, kind) in ACCEPTED {
        let covered = patterns
            .iter()
            .any(|(group, re)| *group == kind_to_vim_group(*kind) && re.is_match(form));
        assert!(
            covered,
            "vim grammar `editors/vim/syntax/silt.vim` has no \
             `syntax match {}` pattern fully matching the \
             lexer-accepted literal `{form}`. Patterns found: {:?}",
            kind_to_vim_group(*kind),
            patterns
                .iter()
                .map(|(g, r)| format!("{g}: {}", r.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn vim_grammar_rejects_every_lexer_rejected_number_form() {
    let patterns = vim_number_patterns();
    for form in REJECTED {
        for (group, re) in &patterns {
            assert!(
                !re.is_match(form),
                "vim grammar pattern `{group}` ({}) fully matches \
                 `{form}`, which the lexer rejects as a number literal",
                re.as_str()
            );
        }
    }
}

/// Vim resolves same-start-column match conflicts in favor of the
/// item defined LAST (`:help syn-priority`). `1.5` starts a valid
/// siltNumber match (`1`) and a valid siltFloat match (`1.5`) at the
/// same column, so every siltFloat definition must come after every
/// siltNumber definition or floats lose their fractional half.
#[test]
fn vim_float_patterns_defined_after_integer_patterns() {
    let vim = read_repo_file("editors/vim/syntax/silt.vim");
    let last_number = vim
        .rfind("syntax match siltNumber")
        .expect("silt.vim must define siltNumber");
    let first_float = vim
        .find("syntax match siltFloat")
        .expect("silt.vim must define siltFloat");
    assert!(
        first_float > last_number,
        "silt.vim defines a siltFloat match before the last siltNumber \
         match; vim gives same-column priority to the LAST definition, \
         so `1.5` would highlight as siltNumber(`1`) — move all \
         siltFloat lines below the siltNumber lines"
    );
}

/// Extract every `syntax match siltNumber|siltFloat "<pattern>"` line
/// and compile the vim pattern as an anchored Rust regex.
fn vim_number_patterns() -> Vec<(&'static str, Regex)> {
    let vim = read_repo_file("editors/vim/syntax/silt.vim");
    let mut out = Vec::new();
    for line in vim.lines() {
        let trimmed = line.trim_start();
        let group = if trimmed.starts_with("syntax match siltNumber") {
            "siltNumber"
        } else if trimmed.starts_with("syntax match siltFloat") {
            "siltFloat"
        } else {
            continue;
        };
        let open = trimmed.find('"').expect("syntax match line missing `\"`");
        let after = &trimmed[open + 1..];
        let close = after
            .find('"')
            .expect("syntax match line missing closing `\"`");
        let anchored = format!("^(?:{})$", vim_to_rust_regex(&after[..close]));
        let re = Regex::new(&anchored).unwrap_or_else(|e| {
            panic!(
                "vim pattern `{}` did not convert to a valid regex: {e}",
                &after[..close]
            )
        });
        out.push((group, re));
    }
    assert!(
        out.iter().any(|(g, _)| *g == "siltNumber") && out.iter().any(|(g, _)| *g == "siltFloat"),
        "silt.vim must define both siltNumber and siltFloat matches; found: {out:?}"
    );
    out
}

fn kind_to_vim_group(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "siltNumber",
        Kind::Float => "siltFloat",
    }
}

/// Convert the vim-regex subset used by the silt.vim number patterns
/// into Rust-regex syntax. Unescaped characters (literals, `[...]`
/// classes, `*`) mean the same thing in both dialects; only the
/// backslash escapes differ. Panics on an escape it does not know so
/// a new vim construct must be handled here explicitly.
fn vim_to_rust_regex(vim: &str) -> String {
    let mut out = String::new();
    let mut chars = vim.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().expect("dangling `\\` in vim pattern") {
            '<' | '>' => out.push_str(r"\b"),
            'd' => out.push_str("[0-9]"),
            '+' => out.push('+'),
            '=' => out.push('?'),
            '(' => out.push_str("(?:"),
            ')' => out.push(')'),
            '.' => out.push_str(r"\."),
            other => panic!(
                "unhandled vim escape `\\{other}` in pattern `{vim}` — \
                 extend vim_to_rust_regex in this test"
            ),
        }
    }
    out
}

// ── VSCode grammar ─────────────────────────────────────────────────

#[test]
fn vscode_grammar_covers_every_lexer_accepted_number_literal() {
    let patterns = vscode_number_patterns();
    for (form, kind) in ACCEPTED {
        let covered = patterns
            .iter()
            .any(|(name, re)| vscode_scope_kind(name) == *kind && re.is_match(form));
        assert!(
            covered,
            "vscode grammar `editors/vscode/syntaxes/silt.tmLanguage.json` \
             has no `numbers` pattern of kind {kind:?} fully matching the \
             lexer-accepted literal `{form}`. Patterns found: {:?}",
            patterns
                .iter()
                .map(|(n, r)| format!("{n}: {}", r.as_str()))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn vscode_grammar_rejects_every_lexer_rejected_number_form() {
    let patterns = vscode_number_patterns();
    for form in REJECTED {
        for (name, re) in &patterns {
            assert!(
                !re.is_match(form),
                "vscode grammar pattern `{name}` ({}) fully matches \
                 `{form}`, which the lexer rejects as a number literal",
                re.as_str()
            );
        }
    }
}

/// TextMate grammars try patterns in array order and take the first
/// match at a position, so the bare-decimal integer pattern must come
/// after the float patterns or `1.5` scopes as integer(`1`).
#[test]
fn vscode_integer_pattern_listed_after_float_patterns() {
    let block = vscode_numbers_block();
    let integer = block
        .find("constant.numeric.integer.silt")
        .expect("numbers block must contain constant.numeric.integer.silt");
    let last_float = block
        .rfind("constant.numeric.float.silt")
        .expect("numbers block must contain constant.numeric.float.silt");
    assert!(
        integer > last_float,
        "vscode numbers block lists constant.numeric.integer.silt before \
         a constant.numeric.float.silt pattern; first-match-wins would \
         scope `1.5` as integer(`1`) — move the integer pattern last"
    );
}

fn vscode_scope_kind(name: &str) -> Kind {
    if name.contains(".float.") {
        Kind::Float
    } else {
        Kind::Int
    }
}

/// Extract `(name, anchored regex)` for every pattern in the vscode
/// grammar's `numbers` repository entry.
fn vscode_number_patterns() -> Vec<(String, Regex)> {
    let block = vscode_numbers_block();
    let pair_re = Regex::new(r#""name":\s*"([^"]+)",\s*"match":\s*"((?:[^"\\]|\\.)*)""#).unwrap();
    let mut out = Vec::new();
    for caps in pair_re.captures_iter(&block) {
        let name = caps[1].to_string();
        let raw = decode_json_string(&caps[2]);
        let anchored = format!("^(?:{raw})$");
        let re = Regex::new(&anchored).unwrap_or_else(|e| {
            panic!("vscode pattern `{name}` regex `{raw}` is not valid Rust-regex: {e}")
        });
        out.push((name, re));
    }
    assert!(
        !out.is_empty(),
        "no name/match pairs extracted from the vscode numbers block"
    );
    out
}

/// Return the `"numbers": { "patterns": [ ... ] }` block of the
/// vscode grammar (from the `"numbers"` key through the matching
/// close of its patterns array).
fn vscode_numbers_block() -> String {
    let vscode = read_repo_file("editors/vscode/syntaxes/silt.tmLanguage.json");
    let start = vscode
        .find("\"numbers\"")
        .expect("tmLanguage grammar must contain a \"numbers\" repository entry");
    let tail = &vscode[start..];
    let patterns_at = tail
        .find("\"patterns\"")
        .expect("\"numbers\" entry must contain a \"patterns\" array");
    let array_open = tail[patterns_at..]
        .find('[')
        .expect("\"patterns\" array opening `[` not found");
    let abs_open = patterns_at + array_open;
    let bytes = tail.as_bytes();
    let mut depth = 0i32;
    let mut i = abs_open;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return tail[..=i].to_string();
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("\"patterns\" array closing `]` not found in numbers block");
}

/// Decode the JSON string escapes that appear in tmLanguage `match`
/// values (`\\` and `\"`). Panics on any other escape so a new one
/// must be handled here explicitly.
fn decode_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().expect("dangling `\\` in JSON string") {
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '/' => out.push('/'),
            other => panic!(
                "unhandled JSON escape `\\{other}` in tmLanguage match \
                 value `{s}` — extend decode_json_string in this test"
            ),
        }
    }
    out
}

// ── Shared helpers ─────────────────────────────────────────────────

fn read_repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}
