//! The formatter: `silt fmt` and the language server's formatting. It
//! follows the token stream.
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

use crate::diagnostic::{Code, Diagnostic};
use crate::lexer::{Lexed, Lexer};
use crate::parser::Parser;
use crate::source::{FileId, Span};

/// The width the printer fills.
pub const WIDTH: usize = 100;

/// What would have gone wrong, phrased to follow "formatting refused: ",
/// and the place in the input it belongs to, when there is one.
#[derive(Debug)]
pub struct Refusal {
    pub message: String,
    pub span: Option<Span>,
}

/// Format `source`, the text of `file`.
///
/// An error is the lexer's or the parser's diagnostic for a text that
/// is not a program, or a refusal ([`Code::FormatRefused`]): the text is
/// fine, but the printer could not follow its tokens, or its result
/// failed the check that every result goes through before it is returned
/// (see `check`): it parses, it is the same program, it spells every
/// literal the same way, and it holds the same comments in the same
/// order. A refusal is a defect of the formatter.
pub fn format(file: FileId, source: &str) -> Result<String, Diagnostic> {
    on_own_stack(file, source, |text| text)
}

/// `format`, with `tamper` applied to the printer's result before the
/// oracle sees it: the way to test that a wrong result is refused and
/// not returned. Not part of a release build.
#[doc(hidden)]
#[cfg(any(test, debug_assertions))]
pub fn format_with(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String + Send + 'static,
) -> Result<String, Diagnostic> {
    on_own_stack(file, source, tamper)
}

/// A refusal as a diagnostic: at its place, or at the first token.
fn refused(file: FileId, lexed: &Lexed, refusal: Refusal) -> Diagnostic {
    let span = refusal
        .span
        .or_else(|| lexed.tokens.first().map(|tok| tok.span))
        .unwrap_or(Span::point(file, 0));
    Diagnostic::error(
        Code::FormatRefused,
        span,
        format!("formatting refused: {}", refusal.message),
    )
    .with_note("the text was left unchanged")
    .with_note(
        "this is a defect in `silt fmt`, not in your program; until it is fixed, moving the \
         comment onto a line of its own or simplifying the expression usually lets the file \
         format",
    )
}

/// The stack `format` works on. The printer and the oracle recurse over
/// the syntax tree, as the checker and the compiler do, and the tree of
/// a chain of 2,000 operators is 2,000 levels deep: `format` works on a
/// thread of its own with the reserve `silt` gives its main thread, so
/// that it does not depend on the stack of its caller (a language
/// server's request thread, a test thread of 1 MiB on Windows). The
/// reserve is address space; only the pages that are touched are
/// committed.
#[cfg(not(target_arch = "wasm32"))]
const STACK: usize = 256 << 20;

/// Lex, parse, print and check on the formatter's thread.
#[cfg(not(target_arch = "wasm32"))]
fn on_own_stack(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String + Send + 'static,
) -> Result<String, Diagnostic> {
    use std::sync::mpsc;

    type Job = Box<dyn FnOnce() + Send>;

    /// The thread that formats for the thread that owns this. Starting
    /// one costs several times what formatting a file does, so it is
    /// kept; it ends when its owner does.
    struct Worker(mpsc::Sender<Job>);

    thread_local! {
        static WORKER: Worker = {
            let (jobs, queue) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("silt-format".into())
                .stack_size(STACK)
                .spawn(move || {
                    for job in queue {
                        job();
                    }
                })
                .expect("spawning the formatter's thread");
            Worker(jobs)
        };
    }

    // The tree and its symbols stay on that thread (the interner is per
    // thread, and each run starts with an empty one); text and
    // diagnostics, which hold no symbol, come back.
    let (reply, result) = mpsc::sync_channel(1);
    let source = source.to_string();
    let job: Job = Box::new(move || {
        crate::intern::reset();
        let run = std::panic::AssertUnwindSafe(|| format_here(file, &source, tamper));
        let _ = reply.send(std::panic::catch_unwind(run));
    });
    WORKER.with(|worker| worker.0.send(job).expect("the formatter's thread is gone"));
    match result.recv().expect("the formatter's thread is gone") {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// On wasm32 there are no threads to give a stack to.
#[cfg(target_arch = "wasm32")]
fn on_own_stack(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String + Send + 'static,
) -> Result<String, Diagnostic> {
    format_here(file, source, tamper)
}

fn format_here(
    file: FileId,
    source: &str,
    tamper: impl FnOnce(String) -> String,
) -> Result<String, Diagnostic> {
    let lexed = Lexer::new(file, source).tokenize()?;
    let program = Parser::new(lexed.clone(), source).parse_program()?;
    let doc = print::program(source, &lexed, &program).map_err(|mismatch| {
        let refusal = Refusal {
            message: mismatch.message,
            span: Some(mismatch.span),
        };
        refused(file, &lexed, refusal)
    })?;
    let mut output = doc::render(&doc, WIDTH);
    // A byte-order mark stays where it is.
    if source.starts_with('\u{feff}') {
        output.insert(0, '\u{feff}');
    }
    let output = tamper(output);
    // Text that is returned unchanged needs no check.
    if output != source {
        check::verify(source, &lexed, &program, &output)
            .map_err(|refusal| refused(file, &lexed, refusal))?;
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
        assert_eq!(error.phase(), crate::diagnostic::Phase::Parse, "{error:?}");
        let error = format(FileId::default(), "fn main() { \"open }").unwrap_err();
        assert_eq!(error.phase(), crate::diagnostic::Phase::Lex, "{error:?}");
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
            Err(refusal) if refusal.code == Code::FormatRefused => refusal.message,
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

    #[test]
    fn a_panic_of_the_printer_is_the_caller_s_and_the_next_text_is_formatted() {
        let source = "fn main() {\n  1\n}\n";
        let panicked = std::panic::catch_unwind(|| {
            format_with(FileId::default(), source, |_| panic!("a defect"))
        });
        let payload = panicked.expect_err("the panic is passed on");
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"a defect"));
        assert_eq!(fmt(source), source);
    }
}
