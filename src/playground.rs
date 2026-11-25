//! What the playground (playground/, the page of silt-lang.com) takes
//! of silt beyond the embedding API: the colours of a text's tokens, the
//! names an editor can offer, a text formatted, and the names of what an
//! analysis hands out. Built with the `playground` feature only; no part
//! of what the crate promises.
//!
//! Offsets here are byte offsets; the playground counts them again in
//! UTF-16 code units for the page.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::ast::{Decl, PatternKind, TypeBody};
use crate::intern::resolve;
use crate::lexer::{KEYWORD_LITERALS, KEYWORDS, Lexer, Tok, Token};
use crate::module;
use crate::session::Session;
use crate::source::{SourceMap, SourceName, Span};
use crate::vm::VmError;

pub use crate::diagnostic::{Diagnostic, Severity};
pub use crate::format::format;
pub use crate::source::FileId;

/// The version of silt this is.
pub const VERSION: &str = crate::VERSION;

/// `diagnostic` as the `silt` command prints it, its places read from
/// `session`'s files.
pub fn render_diagnostic(session: &Session, diagnostic: &Diagnostic) -> String {
    session.render(diagnostic)
}

/// Sort `diagnostics`, of `session`'s files, into the order the `silt`
/// command prints them: by file, then by line, the errors of a line
/// before its warnings, then by place.
pub fn in_print_order(session: &Session, diagnostics: &mut [Diagnostic]) {
    crate::diagnostic::in_print_order(session.sources(), diagnostics)
}

/// The line `silt run` writes for a `main` that returned `Err`, if that
/// ended the program: `error: <text>`, and the note of a `Display` impl
/// of the `Err` that failed.
pub fn returned(session: &Session, error: &VmError) -> Option<String> {
    let text = error.returned.as_deref()?;
    Some(error.returned_text(text, session.sources()))
}

/// `error` as the diagnostic `silt run` prints.
pub fn error_diagnostic(error: &VmError) -> Diagnostic {
    error.to_diagnostic()
}

/// `diagnostic` with the note `note` under its message.
pub fn with_note(diagnostic: Diagnostic, note: impl Into<String>) -> Diagnostic {
    diagnostic.with_note(note)
}

// ── Complete ──────────────────────────────────────────────────────────

/// The names a program can use, each with its kind: the keywords, what
/// silt declares without an import, each builtin module of this build
/// and its members (as `module.member`), and what `file` declares at
/// its top level, also when it has errors. A kind is `"keyword"`,
/// `"function"`, `"constant"`, `"constructor"`, `"module"`, `"type"` or
/// `"variable"`.
pub fn completions(session: &mut Session, file: FileId) -> Vec<(String, &'static str)> {
    let mut items = Vec::new();
    let mut add = |label: String, kind: &'static str| items.push((label, kind));

    for word in KEYWORDS.iter().chain(KEYWORD_LITERALS) {
        add(word.to_string(), "keyword");
    }
    for name in module::builtin_free_function_names() {
        add(name.to_string(), "function");
    }
    for (_, variants) in module::builtin_prelude_enum_variants_with_arity() {
        for (variant, _) in *variants {
            add(variant.to_string(), "constructor");
        }
    }
    for name in module::builtin_modules() {
        if module::missing_feature(name).is_some() {
            continue;
        }
        add(name.to_string(), "module");
        for member in module::builtin_module_functions(name) {
            add(format!("{name}.{member}"), "function");
        }
        for member in module::builtin_module_constants(name) {
            add(format!("{name}.{member}"), "constant");
        }
        for member in module::builtin_module_variants(name) {
            add(format!("{name}.{member}"), "constructor");
        }
    }

    session.analyze(file);
    if let Some(analysis) = session.module_analysis(session.module_of(file)) {
        for decl in &analysis.ast.decls {
            match decl {
                Decl::Fn(f) => add(resolve(f.name), "function"),
                Decl::Type(t) => {
                    add(resolve(t.name), "type");
                    if let TypeBody::Enum(variants) = &t.body {
                        for variant in variants {
                            add(resolve(variant.name), "constructor");
                        }
                    }
                }
                Decl::Trait(t) => add(resolve(t.name), "type"),
                Decl::Let { pattern, .. } => {
                    if let PatternKind::Ident(name) = &pattern.kind {
                        add(resolve(*name), "variable");
                    }
                }
                Decl::TraitImpl(_) | Decl::Import(..) => {}
            }
        }
    }
    items
}

// ── Tokens ────────────────────────────────────────────────────────────

/// The kinds of token [`tokens`] tells apart, by their number. They are
/// the kinds the editors' grammars colour (`editors/`).
pub const TOKEN_KINDS: &[&str] = &[
    "keyword",
    "string",
    "number",
    "literal",
    "type",
    "constructor",
    "builtin",
    "function",
    "operator",
    "punctuation",
    "comment",
];

const KEYWORD: u32 = 0;
const STRING: u32 = 1;
const NUMBER: u32 = 2;
const LITERAL: u32 = 3;
const TYPE: u32 = 4;
const CONSTRUCTOR: u32 = 5;
const BUILTIN: u32 = 6;
const FUNCTION: u32 = 7;
const OPERATOR: u32 = 8;
const PUNCTUATION: u32 = 9;
const COMMENT: u32 = 10;

/// The kind of the name `name`, if it is coloured: the name a `fn`
/// declares (`declared`), a primitive type (`Int`, which `json.Int`
/// also names), a variant silt declares (`Ok`, `Monday`), a function
/// that needs no import (`println`), or a capitalised name: a type, a
/// trait or a variant.
fn name_kind(name: &str, declared: bool) -> Option<u32> {
    static CONSTRUCTORS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let constructors =
        CONSTRUCTORS.get_or_init(|| module::all_builtin_constructor_names().collect());
    if declared {
        Some(FUNCTION)
    } else if module::BUILTIN_PRIMITIVE_NAMES.contains(&name) {
        Some(TYPE)
    } else if constructors.contains(name) {
        Some(CONSTRUCTOR)
    } else if module::builtin_free_function_names().contains(&name) {
        Some(BUILTIN)
    } else if name.starts_with(|c: char| c.is_uppercase()) {
        Some(TYPE)
    } else {
        None
    }
}

/// The kind of `token`, for the tokens that are coloured whatever
/// stands around them; a name is [`name_kind`]'s.
fn kind_of(token: &Token) -> Option<u32> {
    Some(match token {
        Token::Let
        | Token::Fn
        | Token::Type
        | Token::Trait
        | Token::Match
        | Token::When
        | Token::Return
        | Token::Pub
        | Token::Opaque
        | Token::Import
        | Token::As
        | Token::Else
        | Token::Where => KEYWORD,
        Token::Int(_) | Token::Float(_) => NUMBER,
        Token::Bool(_) => LITERAL,
        // The parts of a string with interpolations: each from its
        // quote or brace to its brace or quote.
        Token::StringLit(_)
        | Token::StringStart(_)
        | Token::StringMiddle(_)
        | Token::StringEnd(_) => STRING,
        Token::Plus
        | Token::Minus
        | Token::Star
        | Token::Slash
        | Token::Percent
        | Token::EqEq
        | Token::NotEq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::AndAnd
        | Token::OrOr
        | Token::Not
        | Token::Pipe
        | Token::Bar
        | Token::Question
        | Token::Caret
        | Token::DotDot
        | Token::Arrow => OPERATOR,
        Token::LParen
        | Token::RParen
        | Token::LBrace
        | Token::RBrace
        | Token::LBracket
        | Token::RBracket
        | Token::HashBrace
        | Token::HashBracket
        | Token::Comma
        | Token::Colon
        | Token::Dot
        | Token::Eq => PUNCTUATION,
        // Text that is no token, and the end.
        Token::Ident(_) | Token::Error | Token::Eof => return None,
    })
}

/// The span of `tok`, and for the part of a string behind an
/// interpolation the brace that ends the interpolation too: the lexer's
/// span has the brace that opens one and not the brace that closes it,
/// and a string is coloured with both.
fn string_part(source: &str, tok: &Tok) -> Span {
    let mut span = tok.span;
    let closes = matches!(tok.kind, Token::StringMiddle(_) | Token::StringEnd(_));
    if closes && span.start > 0 && source.as_bytes().get(span.start as usize - 1) == Some(&b'}') {
        span.start -= 1;
    }
    span
}

/// The coloured tokens and the comments of `source`, in the order of
/// the text, as the lexer reads them: where each starts and ends (byte
/// offsets), and its kind (an index of [`TOKEN_KINDS`]). A text with
/// errors has tokens too.
pub fn tokens(source: &str) -> Vec<(u32, u32, u32)> {
    let file = SourceMap::new().add(
        SourceName::Overlay(PathBuf::from("main.silt")),
        source.into(),
    );
    let lexed = Lexer::new(file, source).tokenize();
    let mut spans: Vec<(Span, u32)> = Vec::new();
    let mut after_fn = false;
    for tok in &lexed.tokens {
        let kind = match &tok.kind {
            Token::Ident(name) => name_kind(&resolve(*name), after_fn),
            token => kind_of(token),
        };
        after_fn = tok.kind == Token::Fn;
        if let Some(kind) = kind {
            spans.push((string_part(source, tok), kind));
        }
    }
    spans.extend(lexed.comments.iter().map(|c| (c.span, COMMENT)));
    spans.sort_by_key(|(span, _)| span.start);
    (spans.into_iter())
        .map(|(span, kind)| (span.start, span.end, kind))
        .collect()
}
