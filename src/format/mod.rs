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
/// is returned (see `check`): it parses, it is the same program, and it
/// holds the same comments in the same order.
pub fn format(file: FileId, source: &str) -> Result<String, Error> {
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
    fn a_long_chain_of_operators_is_printed() {
        // The deepest tree the parser accepts.
        let chain = vec!["x"; 1500].join(" + ");
        let source = format!("fn main() {{\n  {chain}\n}}\n");
        let once = fmt(&source);
        assert!(once.lines().count() > 20);
        assert_eq!(fmt(&once), once);
    }

    #[test]
    fn a_long_chain_of_field_accesses_and_calls_is_printed() {
        // The printer recurses over the tree, as the checker and the
        // compiler do: on the stack that `silt` gives its main thread,
        // the deepest chain the parser accepts is printed.
        let on_main_stack = std::thread::Builder::new().stack_size(256 << 20);
        on_main_stack
            .spawn(|| {
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
