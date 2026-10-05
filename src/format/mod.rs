//! The formatter that follows the token stream (stage 8).
//!
//! Not yet what `silt fmt` runs by default: see `src/formatter.rs`.
//!
//! - `doc`: the layout document and its renderer;
//! - `cursor`: the token cursor, which brings every comment;
//! - `print`: syntax tree and tokens to a document;
//! - `check`: the oracle that judges the result.
//!
//! # Why a second pass changes nothing
//!
//! Formatting the result gives the result again. With comments that is
//! not obvious, since a comment may end up behind other tokens than it
//! stood behind. It holds because of four rules, each of which the
//! property runner (`tests/frontend/fmt_property/`) has shown to be
//! needed:
//!
//! 1. What a comment is (at the end of a line, on a line of its own,
//!    between two tokens) is read off the tokens and line breaks around
//!    it, and the printer writes it so that the same is read off the
//!    result (`cursor`).
//! 2. A comment is not part of the layout. It has no width; a `--`
//!    comment breaks only the groups it stands in, and those break
//!    wherever in them the comment stands; where two comments cannot
//!    share a line, the line is broken at the first one before what
//!    follows is laid out (`doc`).
//! 3. The printer never moves a token past a comment. Parentheses with
//!    a comment directly inside them stay, and so does a closure
//!    argument with a comment between it and the call's parentheses
//!    (`print`).
//! 4. A line break is only ever written where the grammar allows one
//!    whatever stands around it, or where the source has one.

pub mod check;
pub mod cursor;
pub mod doc;
pub mod print;

use crate::diagnostic::Diagnostic;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::source::{FileId, Span};

/// The width the printer fills.
pub const WIDTH: usize = 100;

/// Why `format` gave no text.
#[derive(Debug)]
pub enum Error {
    /// The input does not lex or parse.
    Syntax(Diagnostic),
    /// The input is fine, but the printer could not follow its tokens,
    /// or its result failed the oracle. A defect of the formatter.
    Refused(Refusal),
}

/// What would have gone wrong, phrased to follow "formatting refused: ",
/// and the place in the input it belongs to, when there is one.
#[derive(Debug)]
pub struct Refusal {
    pub message: String,
    pub span: Option<Span>,
}

/// Format `source`, the text of `file`. The result is checked before it
/// is returned (see `check`): it parses, it is the same program, it
/// spells every literal the same way, and it holds the same comments in
/// the same order.
pub fn format(file: FileId, source: &str) -> Result<String, Error> {
    format_with(file, source, |text| text)
}

/// The stack `format` works on. The printer and the oracle recurse over
/// the syntax tree, as the checker and the compiler do, and the tree of
/// a chain of 2,000 operators is 2,000 levels deep: `format` runs on a
/// thread of its own with the reserve `silt` gives its main thread, so
/// that it does not depend on the stack of its caller (a language
/// server's request thread, a test thread of 1 MiB on Windows). The
/// reserve is address space; only the pages that are touched are
/// committed.
const STACK: usize = 256 << 20;

/// `format`, with `tamper` applied to the printer's result before the
/// oracle sees it: the way to test that a wrong result is refused and
/// not returned.
#[doc(hidden)]
pub fn format_with(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String + Send,
) -> Result<String, Error> {
    // The tree and its symbols stay on the thread (the interner is per
    // thread); text and diagnostics, which hold none, come back.
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("silt-format".into())
            .stack_size(STACK)
            .spawn_scoped(scope, || format_here(file, source, tamper))
            .expect("spawning the formatter's thread");
        match worker.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

fn format_here(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String,
) -> Result<String, Error> {
    let lexed = Lexer::new(file, source).tokenize().map_err(Error::Syntax)?;
    let program = Parser::new(lexed.clone(), source)
        .parse_program()
        .map_err(Error::Syntax)?;
    let doc = print::program(source, &lexed, &program).map_err(|mismatch| {
        Error::Refused(Refusal {
            message: mismatch.message,
            span: Some(mismatch.span),
        })
    })?;
    let mut output = doc::render(&doc, WIDTH);
    // A byte-order mark stays where it is.
    if source.starts_with('\u{feff}') {
        output.insert(0, '\u{feff}');
    }
    let output = tamper(output);
    // Text that is returned unchanged needs no check.
    if output != source {
        check::verify(source, &lexed, &program, &output).map_err(Error::Refused)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(source: &str) -> String {
        match format(FileId::default(), source) {
            Ok(text) => text,
            Err(e) => panic!("{source:?} is not formatted: {e:?}"),
        }
    }

    #[test]
    fn the_result_is_a_fixed_point() {
        let source = "import b\nimport a\nfn main(){let x=[1,2,]\nprintln(x)}";
        let once = fmt(source);
        assert_eq!(
            once,
            "import a\nimport b\n\nfn main() {\n  let x = [1, 2]\n  println(x)\n}\n"
        );
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn a_file_without_declarations_keeps_its_comments() {
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("\n\n"), "");
        assert_eq!(
            fmt("-- only\n\n\n{- a\n   b -}"),
            "-- only\n\n{- a\n   b -}\n"
        );
    }

    #[test]
    fn a_byte_order_mark_and_carriage_returns() {
        // The mark stays; line ends become line feeds, except inside a
        // string, which is written as it is.
        assert_eq!(
            fmt("\u{feff}fn main() {\r\n  1 -- one\r\n}\r\n"),
            "\u{feff}fn main() {\n  1 -- one\n}\n"
        );
        let text = "fn main() {\n  \"a\r\nb\"\n}\n";
        assert_eq!(fmt(text), text);
    }

    #[test]
    fn a_syntax_error_is_the_parser_s_diagnostic() {
        let error = format(FileId::default(), "fn main() { let }").unwrap_err();
        assert!(matches!(error, Error::Syntax(_)), "{error:?}");
        let error = format(FileId::default(), "fn main() { \"open }").unwrap_err();
        assert!(matches!(error, Error::Syntax(_)), "{error:?}");
    }

    #[test]
    fn a_wrong_result_is_refused_and_not_returned() {
        // The oracle is fed what a defect in the printer would give it.
        let source = "-- Adds one.\nfn inc(x) {\n  x + 0x01 -- one\n}\n";
        let refused = |tamper: fn(String) -> String| match format_with(
            FileId::default(),
            &format!("\n{source}"),
            tamper,
        ) {
            Err(Error::Refused(refusal)) => refusal.message,
            other => panic!("not refused: {other:?}"),
        };
        assert!(refused(|text| text.replace("0x01", "1")).contains("another way"));
        assert!(refused(|text| text.replace(" -- one", "")).contains("lose the comment"));
        assert!(refused(|text| text.replace("x +", "x -")).contains("would not stay"));
        assert!(refused(|text| text.replace("one.\n", "one.\n\n")).contains("the top of the file"));
        assert!(refused(|text| text.replace("x + 0x01", "(x + 0x01")).contains("would not parse"));
        // Untampered, it is formatted.
        assert_eq!(fmt(&format!("\n{source}")), source);
    }

    #[test]
    fn a_long_chain_of_operators_is_printed() {
        // The deepest tree the parser accepts.
        let chain = vec!["x"; 1500].join(" + ");
        let source = format!("fn main() {{\n  {chain}\n}}\n");
        let once = fmt(&source);
        assert!(once.lines().count() > 20);
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn the_deepest_chains_are_printed_whatever_the_caller_s_stack() {
        // `format` brings its own stack: the deepest chains the parser
        // accepts are printed from a thread with 1 MiB, which is what a
        // test thread has on Windows.
        let small = std::thread::Builder::new().stack_size(1 << 20);
        small
            .spawn(|| {
                let chain = vec!["x"; 2000].join(" + ");
                let source = format!("fn main() {{\n  {chain}\n}}\n");
                let once = fmt(&source);
                assert!(once.lines().count() > 20);
                assert_eq!(fmt(&once), once);
                for link in [".f", "()", "?"] {
                    let chain = format!("x{}", link.repeat(2000));
                    let source = format!("fn main() {{\n  {chain}\n}}\n");
                    assert_eq!(fmt(&source), source, "{link}");
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
