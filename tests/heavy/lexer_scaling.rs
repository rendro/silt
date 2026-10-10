//! The lexer is linear in the text whatever is in it, and what it keeps
//! of a text that is no program does not grow with the text.
//!
//! The lexer goes on behind an error, so every rule that reads ahead or
//! back for an error is a place where a hostile text can cost the
//! square of its length: a non-silt quotation mark that looks for its
//! partner on a line of a megabyte took two minutes, and the names of
//! 120,000 distinct non-ASCII words a minute and a half. Each shape
//! here is lexed at one megabyte and at two, on one line and on many:
//! twice the text is about twice the time.

use std::time::{Duration, Instant};

use silt::lexer::{Lexer, MAX_SYNTAX_ERRORS, Token};
use silt::source::FileId;

/// `unit` repeated to at least `bytes` bytes.
fn repeated(unit: &str, bytes: usize) -> String {
    unit.repeat(bytes / unit.len() + 1)
}

/// Distinct words with a letter outside ASCII, `between` between them.
fn distinct_names(between: &str, bytes: usize) -> String {
    let mut text = String::with_capacity(bytes + 16);
    let mut i = 0;
    while text.len() < bytes {
        text.push_str(&format!("é{i}{between}"));
        i += 1;
    }
    text
}

/// The hostile shapes: a name and what makes `bytes` bytes of it.
fn shapes() -> Vec<(&'static str, Box<dyn Fn(usize) -> String>)> {
    fn unit(text: &'static str) -> Box<dyn Fn(usize) -> String> {
        Box::new(move |bytes| repeated(text, bytes))
    }
    vec![
        ("apostrophe pairs on one line", unit("'a' ")),
        ("apostrophe pairs on many lines", unit("'a'\n")),
        ("typographic quotes on one line", unit("“a” ")),
        ("backquotes without partners on a line each", unit("`\n")),
        ("apostrophes, each with a far partner", unit("' x y z w ")),
        (
            "distinct non-ASCII names on one line",
            Box::new(|bytes| distinct_names(" ", bytes)),
        ),
        (
            "distinct non-ASCII names on many lines",
            Box::new(|bytes| distinct_names("\n", bytes)),
        ),
        ("one non-ASCII name again and again", unit("café ")),
        ("one long non-ASCII word", unit("é")),
        (
            "long ASCII names that end outside ASCII",
            unit("abcdefghijklmnopqrstuvwxyzé "),
        ),
        ("semicolons", unit(";")),
        ("semicolons on many lines", unit(";\n")),
        ("junk characters", unit("@$")),
        ("junk characters apart", unit("@ $ ")),
        ("junk characters on many lines", unit("@\n")),
        ("control characters", unit("\u{1}\u{2}\u{7f}")),
        ("control characters apart", unit("\u{1} \u{2}\n")),
        (
            "byte-order marks between declarations",
            unit("fn a() { 1 }\n\u{feff}"),
        ),
        ("numbers that are none", unit("0x 1e ")),
        ("strings with wrong escapes", unit("\"\\q\"\n")),
        (
            "one string of wrong escapes",
            Box::new(|bytes| format!("\"{}\"", repeated("\\q", bytes))),
        ),
        ("backslashes", unit("\\")),
    ]
}

/// The best of three times to lex `text`, and what the lexer kept: the
/// number of tokens and of errors.
fn lexed(text: &str) -> (Duration, usize, usize) {
    let mut best = Duration::MAX;
    let mut kept = (0, 0);
    for _ in 0..3 {
        let started = Instant::now();
        let lexed = Lexer::new(FileId::default(), text).tokenize();
        best = best.min(started.elapsed());
        assert_eq!(lexed.tokens.last().map(|tok| &tok.kind), Some(&Token::Eof));
        kept = (lexed.tokens.len(), lexed.errors.len());
    }
    (best, kept.0, kept.1)
}

#[test]
fn twice_the_hostile_text_takes_about_twice_as_long() {
    const MEGABYTE: usize = 1 << 20;
    let mut slow = Vec::new();
    for (name, make) in shapes() {
        let (one, two) = (make(MEGABYTE), make(2 * MEGABYTE));
        // A machine that was busy during one measurement is measured
        // again; a ratio that is over the cap because of what the lexer
        // does is over it every time.
        let mut over = Vec::new();
        for _ in 0..3 {
            let (small, ..) = lexed(&one);
            let (large, ..) = lexed(&two);
            let ratio = large.as_secs_f64() / small.as_secs_f64();
            if ratio <= 2.6 {
                over.clear();
                break;
            }
            over.push(format!("{large:?} against {small:?} ({ratio:.2})"));
        }
        if !over.is_empty() {
            slow.push(format!("{name}: {}", over.join(", ")));
        }
    }
    assert!(
        slow.is_empty(),
        "two megabytes took more than 2.6 times as long to lex as one, in each of three \
         measurements:\n{}",
        slow.join("\n")
    );
}

#[test]
fn what_is_kept_of_hostile_text_does_not_grow_with_it() {
    const MEGABYTE: usize = 1 << 20;
    for (name, make) in shapes() {
        let (_, tokens, errors) = lexed(&make(MEGABYTE));
        assert!(errors <= MAX_SYNTAX_ERRORS, "{name}: {errors} errors kept");
        // A text with more errors than are kept ends in one token for
        // the rest. (One mistake made again and again is one error, and
        // each place a token: as many as a valid text of the size has.)
        if errors == MAX_SYNTAX_ERRORS {
            assert!(tokens < 1000, "{name}: {tokens} tokens kept");
        }
    }
}
