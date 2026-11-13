//! Invariant checks shared between fuzz targets and regression tests.
//!
//! The fuzz targets under `fuzz/fuzz_targets/` historically only asserted
//! "must not panic" or "must be idempotent", which lets a large class of
//! real bugs slip through (dropped tokens, corrupted spans, deleted
//! comments). This module factors the cheap-to-evaluate structural
//! invariants into helper functions so both the fuzzers and the test
//! suite can exercise them on the same inputs.
//!
//! All functions return `Result<(), String>` with a human-readable
//! description on failure; the fuzz targets `unwrap()` that result to
//! trigger a libFuzzer-visible panic, while the regression tests match on
//! the `Err` to verify that synthetic corruption is detected.
//!
//! This module is deliberately kept side-effect-free so it stays safe to
//! call from `no_main` fuzz drivers. Its checks use the lexer, the
//! parser and the formatter, and, for `json.parse`, the decoder.

use crate::ast::{Decl, Program};
use crate::diagnostic::{Code, Diagnostic};
use crate::lexer::{Lexed, Tok, Token};
use crate::source::Span;

/// Verify structural invariants on what `Lexer::tokenize` makes of any
/// text, with or without lex errors.
///
/// Current checks:
///
/// 1. Every span ends no earlier than it starts and no later than
///    `source.len()` (tokens cannot point past the end of the input).
/// 2. Span starts are monotonically non-decreasing across the token
///    stream (the lexer never rewinds).
/// 3. A token does not start before the previous token ends: tokens do
///    not overlap.
/// 4. Exactly one `Eof` token is emitted, and it is the final token.
/// 5. The final `Eof` span offset equals the source length in bytes
///    (the lexer consumed everything).
/// 6. The comment ranges of the tokens follow each other and leave no
///    comment out, and each comment lies between the token before it
///    and the token that carries it.
/// 7. A `Token::Error` has an error: the lexer recorded at least one,
///    and every error it recorded lies in the source.
/// 8. At most `MAX_SYNTAX_ERRORS` errors are kept; when more are
///    counted, that many are kept and the tokens end in one
///    `Token::Error` for the rest of the text.
pub fn check_lexer_invariants(source: &str, lexed: &Lexed) -> Result<(), String> {
    let tokens = &lexed.tokens;
    if tokens.is_empty() {
        return Err("token stream is empty (expected at least Eof)".into());
    }

    let src_len = source.len();
    let mut prev: Option<&Span> = None;
    let mut seen_eof = false;

    for (
        idx,
        Tok {
            kind: tok, span, ..
        },
    ) in tokens.iter().enumerate()
    {
        if seen_eof {
            return Err(format!("token {tok:?} at index {idx} emitted after Eof"));
        }

        if span.end < span.start || span.end as usize > src_len {
            return Err(format!(
                "token {tok:?} at index {idx} has span {}..{} beyond source length {}",
                span.start, span.end, src_len
            ));
        }

        if let Some(p) = prev {
            if span.start < p.start {
                return Err(format!(
                    "token {tok:?} at index {idx} has non-monotonic offset {} < {}",
                    span.start, p.start
                ));
            }
            if span.start < p.end {
                return Err(format!(
                    "token {tok:?} at index {idx} starts at {} inside the previous token (ends at {})",
                    span.start, p.end
                ));
            }
        }

        if matches!(tok, Token::Eof) {
            seen_eof = true;
            if span.start as usize != src_len {
                return Err(format!(
                    "Eof span offset {} != source length {}",
                    span.start, src_len
                ));
            }
        }

        prev = Some(span);
    }

    if !seen_eof {
        return Err("token stream ended without emitting Eof".into());
    }

    let mut next_comment = 0;
    let mut floor = 0;
    for (idx, tok) in tokens.iter().enumerate() {
        if tok.comments.start != next_comment || tok.comments.end < tok.comments.start {
            return Err(format!(
                "token {:?} at index {idx} has comments {:?}, expected a range from {next_comment}",
                tok.kind, tok.comments
            ));
        }
        let Some(comments) = lexed
            .comments
            .get(tok.comments.start as usize..tok.comments.end as usize)
        else {
            return Err(format!(
                "token {:?} at index {idx} has comments {:?} of {}",
                tok.kind,
                tok.comments,
                lexed.comments.len()
            ));
        };
        for comment in comments {
            if comment.span.start < floor || comment.span.end < comment.span.start {
                return Err(format!(
                    "comment at {}..{} starts before offset {floor}",
                    comment.span.start, comment.span.end
                ));
            }
            floor = comment.span.end;
        }
        if tok.span.start < floor {
            return Err(format!(
                "token {:?} at index {idx} starts at {} inside a comment (ends at {floor})",
                tok.kind, tok.span.start
            ));
        }
        floor = tok.span.end;
        next_comment = tok.comments.end;
    }
    if next_comment as usize != lexed.comments.len() {
        return Err(format!(
            "{} comments, of which the tokens carry {next_comment}",
            lexed.comments.len()
        ));
    }

    if lexed.errors.is_empty() && tokens.iter().any(|tok| tok.kind == Token::Error) {
        return Err("an Error token without a lex error".into());
    }
    if lexed.errors.len() > crate::lexer::MAX_SYNTAX_ERRORS {
        return Err(format!("{} lex errors kept", lexed.errors.len()));
    }
    if lexed.more_errors > 0 {
        let rest = tokens.len().checked_sub(2).map(|i| &tokens[i]);
        if lexed.errors.len() != crate::lexer::MAX_SYNTAX_ERRORS
            || !rest.is_some_and(|tok| tok.kind == Token::Error && tok.span.end as usize == src_len)
        {
            return Err(format!(
                "{} errors counted behind {} kept, and the tokens do not end in one Error \
                 token for the rest",
                lexed.more_errors,
                lexed.errors.len()
            ));
        }
    }
    for error in &lexed.errors {
        if error.span.end < error.span.start || error.span.end as usize > src_len {
            return Err(format!(
                "lex error at {}..{} beyond source length {src_len}",
                error.span.start, error.span.end
            ));
        }
    }

    Ok(())
}

/// The tokens that a declaration is made of: all but `Eof` and the
/// parentheses.
fn significant_token_count(tokens: &[Tok]) -> usize {
    tokens
        .iter()
        .filter(|tok| !matches!(tok.kind, Token::Eof | Token::LParen | Token::RParen))
        .count()
}

/// Extract the declaration's top-level span. Used by
/// [`check_parser_invariants`] to validate that every `Decl` points into
/// the source buffer the parser was given (not past the end).
fn decl_span(decl: &Decl) -> Span {
    match decl {
        Decl::Fn(f) => f.span,
        Decl::Type(t) => t.span,
        Decl::Trait(t) => t.span,
        Decl::TraitImpl(i) => i.span,
        Decl::Import(_, span) => *span,
        Decl::Let { span, .. } => *span,
    }
}

/// Verify structural invariants on a successful `Parser::parse_program`
/// result. The caller must have already lexed `source` into `tokens`
/// and produced `program` by calling `Parser::new(tokens, source).parse_program()`.
///
/// Current checks:
///
/// 1. Every top-level `Decl`'s span ends at or before `source.len()`. A
///    formatter or parser that silently corrupts spans would otherwise
///    slip past the other invariants — the fuzzer can't see AST fields
///    directly, but it can see a panic on this assertion.
/// 2. If the source contains any "significant" token (excluding
///    `Eof`, `LParen`, `RParen`), then `program.decls` must
///    be non-empty. A parser bug that silently drops every top-level
///    construct would otherwise produce an empty-but-Ok program.
///    Conversely, empty/whitespace-only source must yield zero decls.
/// 3. The number of decls is bounded by the number of tokens — trivially
///    true for a correct parser, but catches pathological duplication
///    bugs (accidental push-in-a-loop).
pub fn check_parser_invariants(
    source: &str,
    lexed: &Lexed,
    program: &Program,
) -> Result<(), String> {
    let src_len = source.len();
    for (idx, decl) in program.decls.iter().enumerate() {
        let span = decl_span(decl);
        if span.end as usize > src_len {
            return Err(format!(
                "decl at index {idx} has span end {} beyond source length {}",
                span.end, src_len
            ));
        }
    }

    let tokens = &lexed.tokens;
    let sig = significant_token_count(tokens);
    if sig == 0 && !program.decls.is_empty() {
        return Err(format!(
            "empty-of-tokens source produced {} decls",
            program.decls.len()
        ));
    }
    if sig > 0 && program.decls.is_empty() {
        return Err(format!(
            "source has {sig} significant tokens but program has zero decls"
        ));
    }

    if program.decls.len() > tokens.len() {
        return Err(format!(
            "decl count {} exceeds token count {}",
            program.decls.len(),
            tokens.len()
        ));
    }

    Ok(())
}

/// What a correct formatter upholds on `source`, with `format` as the
/// formatter (`crate::format::format`, or a wrong one in a test):
///
/// 1. it does not refuse a text that lexes and parses: a refusal says
///    that the printer could not follow the tokens, or that the result
///    failed the formatter's own check (it would not parse, would be
///    another program, would spell a literal another way or would not
///    hold the same comments in the same order);
/// 2. the result is a fixed point: formatting it gives it again.
///
/// A text that is not a program is no input: `Ok`.
pub fn check_formatter_invariants_of(
    source: &str,
    format: impl Fn(&str) -> Result<String, Diagnostic>,
) -> Result<(), String> {
    let refused = |pass: &str, e: Diagnostic| format!("{pass} pass: {}", e.message);
    let first = match format(source) {
        Ok(first) => first,
        Err(e) if e.code == Code::FormatRefused => return Err(refused("first", e)),
        Err(_) => return Ok(()),
    };
    let second = match format(&first) {
        Ok(second) => second,
        Err(e) if e.code == Code::FormatRefused => return Err(refused("second", e)),
        Err(e) => return Err(format!("the result does not parse: {}", e.message)),
    };
    if first != second {
        return Err(format!(
            "formatter not idempotent: first pass {} bytes, second pass {} bytes",
            first.len(),
            second.len()
        ));
    }
    Ok(())
}

/// [`check_formatter_invariants_of`] for the formatter.
pub fn check_formatter_invariants(source: &str) -> Result<(), String> {
    check_formatter_invariants_of(source, |text| {
        crate::format::format(crate::source::FileId::default(), text)
    })
}

// ── The decoders: json.parse, json.decode and toml.parse on any text ──

/// The types [`check_json_decoding`] decodes a text into: the
/// fields of `Targets`.
const JSON_TYPES: &str = "import json
import time
type Tag { name: String, weight: Int }
type Rec { id: Int, name: String, score: Float, tags: List(Tag), note: Option(String), ok: Bool }
type Box(t) { value: t, more: Option(Box(t)), extra: List(t) }
type Shape { Dot, Circle(Float), Rect(Int, Int), Named(String, Option(Tag)) }
type Either(a, b) { Left(a), Right(b) }
type Node { value: Int, children: List(Node) }
type When { day: time.Date, at: time.Time, stamp: time.DateTime }
type Targets {
  int: Int, float: Float, string: String, bool: Bool,
  maybe: Option(Int), maybe2: Option(Option(Float)),
  ints: List(Int), floats: List(Float), pair: (Int, String), triple: (Bool, Float, Option(Int)),
  map: Map(String, Int), maps: Map(String, List(Tag)),
  value: json.Value, values: Map(String, json.Value),
  tag: Tag, recs: List(Rec), boxed: Box(Int), boxes: Map(String, Box(Shape)),
  shape: Shape, shapes: List(Shape), either: Either(Int, Tag), eithers: List(Either(Shape, Box(Int))),
  node: Node, moment: When, moments: List(When), mix: Map(String, Option(Rec)),
}
";

/// A runtime that knows [`JSON_TYPES`], and the field types of
/// `Targets`.
struct JsonTargets {
    vm: crate::vm::Vm,
    targets: Vec<(String, crate::typeinfo::FieldType)>,
    /// The types a written impl decodes, of [`IMPLS`], each with the
    /// slot of its `decode`.
    impls: Vec<(crate::defs::TypeId, u16)>,
}

/// The types of [`JSON_TYPES`] that a written `json.Decode` impl
/// decodes where a decoding honours impls ([`fake_impl`] stands for
/// each).
const IMPLS: [&str; 3] = ["Tag", "Shape", "Box"];

impl JsonTargets {
    fn new() -> JsonTargets {
        let program = crate::session::testing::compile_decls_str(JSON_TYPES)
            .unwrap_or_else(|e| panic!("the types compile: {}", e[0].message));
        let vm = crate::vm::Vm::new(crate::vm::HostIo::process());
        for info in program.types.iter() {
            vm.runtime.types.lock().insert(info.clone());
        }
        let targets = (program.types.iter())
            .find(|info| info.name == "Targets")
            .expect("Targets is described")
            .fields()
            .to_vec();
        let impls = (program.types.iter())
            .filter_map(|info| {
                let slot = IMPLS.iter().position(|name| *name == info.name)?;
                Some((info.id, 100 + slot as u16))
            })
            .collect();
        JsonTargets { vm, targets, impls }
    }

    /// The slot of the written impl of the type `id`, if [`IMPLS`] has
    /// one.
    fn written(&self, id: crate::defs::TypeId, _: &[crate::typeinfo::FieldType]) -> Option<u16> {
        (self.impls.iter()).find_map(|(of, slot)| (*of == id).then_some(*slot))
    }
}

/// What the written impl of a part gives for its `json.Value` `json`:
/// the text of the value, or, for one of a text a multiple of four
/// long, a `JsonError` that says so.
fn fake_impl(json: &crate::value::Value) -> Result<crate::value::Value, crate::value::Value> {
    use crate::typeinfo::bv;
    use crate::value::Value;
    let text = json.to_string();
    match text.len() % 4 {
        0 => Err(Value::variant(
            bv::JSON_OTHER,
            [Value::string(format!("no {text}"))],
        )),
        _ => Ok(Value::string(text)),
    }
}

/// The impls' calls of a decoding, as the frame makes them for the parts
/// `holes`: in order, each one's slot, type and `json.Value`, up to the
/// first that gives an `Err`; and what they gave.
fn impl_calls<'h>(
    holes: impl IntoIterator<Item = &'h crate::builtins::decode::Hole>,
) -> (
    Vec<String>,
    Vec<Result<crate::value::Value, crate::value::Value>>,
) {
    let mut calls = Vec::new();
    let mut given = Vec::new();
    for hole in holes {
        calls.push(format!("{} {} {}", hole.slot, hole.ty, hole.json));
        let gave = fake_impl(&hole.json);
        let failed = gave.is_err();
        given.push(gave);
        if failed {
            break;
        }
    }
    (calls, given)
}

/// A decoding by the reading of a document as the VM's frame runs it
/// (two readings, with the impls between them; the first only finding
/// the parts where `only_finding`): the impls' calls and the result.
/// `read` reads the document, or gives the call's result where there is
/// none.
fn read_decoding(
    only_finding: bool,
    rt: &crate::vm::Runtime,
    written: &dyn Fn(crate::defs::TypeId, &[crate::typeinfo::FieldType]) -> Option<u16>,
    rules: &crate::builtins::decode::Rules,
    read: impl Fn(
        &crate::builtins::decode::Reader,
        &crate::builtins::decode::seed::Parts,
    ) -> Result<crate::builtins::decode::seed::Outcome, crate::value::Value>,
) -> (Vec<String>, crate::value::Value) {
    use crate::builtins::decode::seed::Parts;
    use crate::builtins::decode::{self, Holes, Reader};
    let reader = Reader::with_impls(rt, written, Holes::Collect(Vec::new()));
    let finding = match only_finding {
        true => Parts::only_finding(),
        false => Parts::finding(),
    };
    let decoded = match read(&reader, &finding) {
        Ok(decoded) => decoded,
        Err(result) => return (Vec::new(), result),
    };
    let built = finding.built();
    let (found, keep_fields) = finding.into_found();
    if found.is_empty() {
        let decoded = match (built, decoded) {
            (false, Ok(_)) => {
                read(&reader, &Parts::supplying(&[], Vec::new(), keep_fields)).expect("a document")
            }
            (_, decoded) => decoded,
        };
        return (Vec::new(), decode::result(rules, decoded));
    }
    let (calls, given) = impl_calls(found.iter().map(|found| &found.hole));
    if let (Ok(_), Err(e)) = (given.last().expect("a call").as_ref(), decoded) {
        return (calls, decode::result(rules, Err(e)));
    }
    let numbers: Vec<usize> = found.iter().map(|found| found.number).collect();
    let supplying = Parts::supplying(&numbers, given, keep_fields);
    let decoded = read(&reader, &supplying).expect("a document");
    (calls, decode::result(rules, decoded))
}

/// `Err` where two decodings of `what` differ, `how` they were made: in
/// the impls they call, or in their result, its text or as a value.
fn same(
    name: &str,
    how: &str,
    first: &(Vec<String>, crate::value::Value),
    second: &(Vec<String>, crate::value::Value),
    what: &str,
) -> Result<(), String> {
    if first.0 != second.0 {
        return Err(format!(
            "{name}: the impls called {:?}, {how} {:?}, of {what}",
            first.0, second.0
        ));
    }
    match first.1.to_string() == second.1.to_string() && first.1 == second.1 {
        true => Ok(()),
        false => Err(format!(
            "{name}: the decoding gives {}, {how} {}, of {what}",
            first.1, second.1
        )),
    }
}

/// `json.parse` of the text `text` into a value of each of a set of
/// types (records, enums and generic types of them, lists, maps,
/// tuples, options, dates and times, `json.Value`), with and without
/// written impls of some of them (`IMPLS`, `fake_impl`), as the VM's
/// frame runs it: it ends (no panic), and ends alike when it runs again
/// (no reading leaves anything behind); and where the text is a
/// document, `json.decode` of the `json.Value` it holds decodes alike:
/// the same value or error, the impls called with the same parts in the
/// same order.
pub fn check_json_decoding(text: &[u8]) -> Result<(), String> {
    use crate::builtins::decode::{Holes, Reader, seed};
    use crate::builtins::json;
    use crate::typeinfo::FieldType;
    thread_local! {
        static TARGETS: JsonTargets = JsonTargets::new();
    }
    TARGETS.with(|targets| {
        let rt = &targets.vm.runtime;
        let none = |_, _: &[FieldType]| None;
        let impls = |id, args: &[FieldType]| targets.written(id, args);
        let plain = Reader::with_impls(rt, &none, Holes::Collect(Vec::new()));
        let finding = seed::Parts::finding();
        let value = match seed::json_text(&plain, &FieldType::JsonValue, text, &finding) {
            Ok(Ok(value)) => Some(value),
            _ => None,
        };
        let shown = String::from_utf8_lossy(text);
        for (name, expected) in &targets.targets {
            for (with_impls, written) in [&none as &dyn Fn(_, &[_]) -> _, &impls]
                .into_iter()
                .enumerate()
            {
                let only_finding = with_impls == 1;
                let from_text = |r: &Reader, parts: &seed::Parts| {
                    seed::json_text(r, expected, text, parts)
                        .map_err(|e| json::json_result_err(&e, text))
                };
                let first = read_decoding(only_finding, rt, written, &json::RULES, from_text);
                let again = read_decoding(only_finding, rt, written, &json::RULES, from_text);
                same(name, "again", &first, &again, &shown)?;
                if let Some(value) = &value {
                    let read =
                        read_decoding(only_finding, rt, written, &json::RULES, |r, parts| {
                            seed::json_value(r, expected, value, parts)
                                .map_err(json::json_unknown_err)
                        });
                    same(name, "its json.Value", &first, &read, &shown)?;
                }
            }
        }
        Ok(())
    })
}

/// `toml.parse`, `toml.parse_map` and `toml.parse_list` of the text
/// `text` into a value of each of the types of [`check_json_decoding`],
/// with and without written impls, as the VM's frame runs them: each
/// ends (no panic), and ends alike when it runs again.
pub fn check_toml_decoding(text: &str) -> Result<(), String> {
    use crate::builtins::decode::seed;
    use crate::builtins::toml;
    thread_local! {
        static TARGETS: JsonTargets = JsonTargets::new();
    }
    TARGETS.with(|targets| {
        let rt = &targets.vm.runtime;
        let none = |_, _: &[crate::typeinfo::FieldType]| None;
        let impls = |id, args: &[crate::typeinfo::FieldType]| targets.written(id, args);
        let keys = |n: usize| format!("{n} keys");
        for (name, expected) in &targets.targets {
            for top in [
                seed::Top::One(expected),
                seed::Top::Map(expected),
                seed::Top::List(expected, &keys),
            ] {
                for (with_impls, written) in [&none as &dyn Fn(_, &[_]) -> _, &impls]
                    .into_iter()
                    .enumerate()
                {
                    let only_finding = with_impls == 1;
                    let read = |r: &crate::builtins::decode::Reader, parts: &seed::Parts| {
                        toml::read_text(r, &top, text, parts)
                    };
                    let first = read_decoding(only_finding, rt, written, &toml::RULES, read);
                    let again = read_decoding(only_finding, rt, written, &toml::RULES, read);
                    same(name, "again", &first, &again, text)?;
                }
            }
        }
        Ok(())
    })
}

/// A TOML text made of `bytes`, which choose each part of it: keys and
/// tables of the shapes of the types [`check_toml_decoding`]
/// decodes, arrays of tables, inline tables, quoted keys, keys written
/// twice, every kind of date and time literal and ones that are no day
/// or time, integers and floats at their edges (`nan`, `inf`), and now
/// and then broken text.
pub fn toml_text_of(bytes: &[u8]) -> String {
    let mut maker = TomlMaker {
        bytes,
        at: 0,
        out: String::new(),
    };
    let lines = 1 + usize::from(maker.next() % 8);
    for _ in 0..lines {
        maker.line();
    }
    maker.out
}

/// What [`toml_text_of`] makes a text with.
struct TomlMaker<'b> {
    bytes: &'b [u8],
    at: usize,
    out: String,
}

impl TomlMaker<'_> {
    const KEYS: &'static [&'static str] = &[
        "id",
        "name",
        "score",
        "tags",
        "note",
        "ok",
        "weight",
        "value",
        "more",
        "extra",
        "children",
        "day",
        "at",
        "stamp",
        "variant",
        "fields",
        "a",
        "b",
        "\"a b\"",
        "\"\"",
        "int",
        "float",
        "string",
        "bool",
        "maybe",
        "ints",
        "pair",
        "map",
        "tag",
        "recs",
        "boxed",
        "shape",
        "shapes",
        "node",
        "moment",
        "mix",
        "\"$__toml_private_datetime\"",
    ];
    const VALUES: &'static [&'static str] = &[
        "0",
        "-1",
        "+7",
        "9223372036854775807",
        "-9223372036854775808",
        "0x10",
        "1_000",
        "1.0",
        "1e3",
        "-0.0",
        "6.626e-34",
        "nan",
        "inf",
        "-inf",
        "+inf",
        "true",
        "false",
        "\"a\"",
        "'lit'",
        "\"Dot\"",
        "\"Rect\"",
        "\"Circle\"",
        "\"1979-05-27\"",
        "\"07:32:00\"",
        "\"\"\"multi\nline\"\"\"",
        "1979-05-27T07:32:00Z",
        "1979-05-27T00:32:00.999999-07:00",
        "1979-05-27 07:32:00Z",
        "1979-05-27T07:32:00",
        "1979-05-27",
        "07:32:00",
        "00:32:00.5",
        "1979-02-30",
        "24:00:00",
        "1979-02-29T25:00:00",
        "2016-12-31T23:59:60Z",
        "[]",
        "{}",
    ];

    fn next(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    fn pick(&mut self, from: &[&'static str]) -> &'static str {
        from[usize::from(self.next()) % from.len()]
    }

    fn line(&mut self) {
        match self.next() % 10 {
            0 => {
                let key = self.pick(Self::KEYS);
                self.out.push_str(&format!("[{key}]\n"));
            }
            1 => {
                let key = self.pick(Self::KEYS);
                self.out.push_str(&format!("[[{key}]]\n"));
            }
            2 => {
                let (a, b) = (self.pick(Self::KEYS), self.pick(Self::KEYS));
                self.out.push_str(&format!("{a}.{b} = "));
                self.value(0);
                self.out.push('\n');
            }
            3 => {
                let broken = self.pick(&["= 1", "a = ", "[x", "a = [1,", "a = {b = 1", "a = 1 2"]);
                self.out.push_str(broken);
                self.out.push('\n');
            }
            _ => {
                let key = self.pick(Self::KEYS);
                self.out.push_str(&format!("{key} = "));
                self.value(0);
                self.out.push('\n');
            }
        }
    }

    fn value(&mut self, depth: usize) {
        match self.next() % 8 {
            0 | 1 if depth < 4 => {
                let n = self.next() % 4;
                self.out.push('[');
                for k in 0..n {
                    if k > 0 {
                        self.out.push_str(", ");
                    }
                    self.value(depth + 1);
                }
                self.out.push(']');
            }
            2 if depth < 4 => {
                let n = self.next() % 4;
                self.out.push('{');
                for k in 0..n {
                    if k > 0 {
                        self.out.push(',');
                    }
                    let key = self.pick(Self::KEYS);
                    self.out.push_str(&format!(" {key} = "));
                    self.value(depth + 1);
                }
                self.out.push_str(" }");
            }
            // A variant with fields, its two keys in either order.
            3 if depth < 4 => {
                let name = self.pick(&[
                    "\"Rect\"",
                    "\"Circle\"",
                    "\"Named\"",
                    "\"Dot\"",
                    "1979-05-27",
                ]);
                let n = self.next() % 3;
                let mut fields = String::from("[");
                for k in 0..n {
                    if k > 0 {
                        fields.push_str(", ");
                    }
                    let value = self.pick(Self::VALUES);
                    fields.push_str(value);
                }
                fields.push(']');
                match self.next() % 2 {
                    0 => self
                        .out
                        .push_str(&format!("{{ variant = {name}, fields = {fields} }}")),
                    _ => self
                        .out
                        .push_str(&format!("{{ fields = {fields}, variant = {name} }}")),
                }
            }
            _ => {
                let value = self.pick(Self::VALUES);
                self.out.push_str(value);
            }
        }
    }
}

/// A JSON text made of `bytes`, which choose each part of it: values of
/// the shapes of the types [`check_json_decoding`] decodes, with
/// their keys, variant names, dates and numbers at the edges of `Int`,
/// keys written twice, nesting past the parser's limit, and now and
/// then broken text.
pub fn json_text_of(bytes: &[u8]) -> Vec<u8> {
    let mut maker = JsonMaker {
        bytes,
        at: 0,
        out: Vec::new(),
    };
    maker.value(0);
    maker.out
}

/// What [`json_text_of`] makes a text with.
struct JsonMaker<'b> {
    bytes: &'b [u8],
    at: usize,
    out: Vec<u8>,
}

impl JsonMaker<'_> {
    const NUMBERS: &'static [&'static str] = &[
        "0",
        "-0",
        "1",
        "-1",
        "7",
        "1.0",
        "1e3",
        "1E+2",
        "-1.5e-3",
        "1.5",
        "0.1e1",
        "50e-1",
        "9223372036854775807",
        "-9223372036854775808",
        "9223372036854775808",
        "-9223372036854775809",
        "18446744073709551615",
        "18446744073709551616",
        "9223372036854775807.0",
        "9223372036854775808.0",
        "1e19",
        "-0.0",
        "1e400",
        "2.5e-400",
        "9007199254740993",
        "9007199254740993.0",
        "123456789012345678901234567890",
        "3.25",
        "92233720368547758070e-1",
        "0.5e1",
        "1e-7",
    ];
    const STRINGS: &'static [&'static str] = &[
        "",
        "a",
        "t",
        "Dot",
        "Circle",
        "Rect",
        "Named",
        "Left",
        "Right",
        "Nope",
        "2026-01-02",
        "12026-01-02",
        "2026-02-30",
        "13:14:15.5",
        "23:59:60",
        "12:30",
        "2026-10-19T13:14:15.5",
        "2026-10-19 13:14:15+02:00",
        "2026-10-19",
        "a b",
        "\u{e9}",
        "\\u00e9",
        "\\\"q\\\"",
        "x\\ny",
        "\\ud83d\\ude00",
    ];
    const KEYS: &'static [&'static str] = &[
        "id", "name", "score", "tags", "note", "ok", "weight", "value", "more", "extra",
        "children", "day", "at", "stamp", "variant", "fields", "a", "b", "a b", "", "x", "int",
        "\\u0069d",
    ];

    fn next(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    fn pick(&mut self, from: &[&str]) -> String {
        let k = usize::from(self.next()) % from.len();
        from[k].to_string()
    }

    fn string(&mut self, from: &[&str]) {
        let s = self.pick(from);
        self.out.push(b'"');
        self.out.extend_from_slice(s.as_bytes());
        self.out.push(b'"');
    }

    fn value(&mut self, depth: usize) {
        let spaced = self.next().is_multiple_of(8);
        if spaced {
            self.out.extend_from_slice(b" \n\t");
        }
        let exhausted = self.at >= self.bytes.len();
        match self.next() % 16 {
            _ if exhausted && depth > 0 => self.out.extend_from_slice(b"1"),
            0 => self.out.extend_from_slice(b"null"),
            1 => self.out.extend_from_slice(b"true"),
            2 => self.out.extend_from_slice(b"false"),
            3 | 4 => {
                let n = self.pick(Self::NUMBERS);
                self.out.extend_from_slice(n.as_bytes());
            }
            5 | 6 => self.string(Self::STRINGS),
            7 | 8 if depth < 6 => {
                let n = self.next() % 5;
                self.out.push(b'[');
                for k in 0..n {
                    if k > 0 {
                        self.out.push(b',');
                    }
                    self.value(depth + 1);
                }
                self.out.push(b']');
            }
            9..=11 if depth < 6 => {
                let n = self.next() % 6;
                self.out.push(b'{');
                for k in 0..n {
                    if k > 0 {
                        self.out.push(b',');
                    }
                    self.string(Self::KEYS);
                    self.out.push(b':');
                    self.value(depth + 1);
                }
                self.out.push(b'}');
            }
            // A variant with fields, its two keys in either order.
            12 if depth < 6 => {
                let fields_first = self.next().is_multiple_of(2);
                self.out.push(b'{');
                for k in 0..2 {
                    if k > 0 {
                        self.out.push(b',');
                    }
                    match (k == 0) == fields_first {
                        true => {
                            self.out.extend_from_slice(b"\"fields\":[");
                            let n = self.next() % 4;
                            for i in 0..n {
                                if i > 0 {
                                    self.out.push(b',');
                                }
                                self.value(depth + 1);
                            }
                            self.out.push(b']');
                        }
                        false => {
                            self.out.extend_from_slice(b"\"variant\":");
                            self.string(Self::STRINGS);
                        }
                    }
                }
                self.out.push(b'}');
            }
            // Nesting near the parser's limit.
            13 if depth == 0 => {
                let levels = 120 + usize::from(self.next() % 16);
                self.out.extend(std::iter::repeat_n(b'[', levels));
                self.value(depth + 1);
                self.out.extend(std::iter::repeat_n(b']', levels));
            }
            // Broken text.
            14 => {
                let broken: &[u8] = match self.next() % 6 {
                    0 => b"[1,]",
                    1 => b"{\"a\":}",
                    2 => b"[1 2]",
                    3 => b"\"open",
                    4 => b"tru",
                    _ => b"{\"a\" 1}",
                };
                self.out.extend_from_slice(broken);
            }
            _ => {
                let n = self.pick(Self::NUMBERS);
                self.out.extend_from_slice(n.as_bytes());
            }
        }
        if spaced {
            self.out.push(b' ');
        }
    }
}

// ── The files of a package ─────────────────────────────────────────

/// Whether `range` is a place in `text`: in order, inside it, and on the
/// boundaries of its characters.
fn place_in(text: &str, (start, end): (usize, usize)) -> bool {
    start <= end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)
}

/// What the readers of a package's files make of any text, as
/// `silt.toml` and as `silt.lock` (a git dependency's files are
/// fetched: their text is anybody's). Each ends without a panic, with an
/// error's place inside the text; and a lockfile that reads is written
/// to a text that reads back as the same lockfile, and is written alike
/// again.
pub fn check_package_files(text: &str) -> Result<(), String> {
    use crate::lockfile::{parse_lockfile, render_lockfile};
    let mut sources = crate::source::SourceMap::new();
    let path = std::path::PathBuf::from("silt.toml");
    if let Err(d) = crate::manifest::Manifest::read(path, text.to_string(), &mut sources) {
        let (start, end) = (d.span.start as usize, d.span.end as usize);
        if !place_in(text, (start, end)) {
            return Err(format!(
                "the manifest error {:?} is at {start}..{end}, not in the {} bytes of the text",
                d.message,
                text.len()
            ));
        }
    }
    let lock = match parse_lockfile(text) {
        Err(e) if place_in(text, e.span) => return Ok(()),
        Err(e) => {
            return Err(format!(
                "the lockfile error is at {:?}, not in the {} bytes of the text",
                e.span,
                text.len()
            ));
        }
        Ok(lock) => lock,
    };
    let written = render_lockfile(&lock);
    let again = parse_lockfile(&written).map_err(|e| {
        format!(
            "a lockfile that read is written to {written:?}, which does not read: {:?}",
            e.span
        )
    })?;
    let by_name = |lock: &crate::lockfile::Lockfile| {
        let mut packages = lock.packages.clone();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        (lock.version, packages)
    };
    if by_name(&again) != by_name(&lock) {
        return Err(format!(
            "a lockfile written to {written:?} reads back as another: {again:?}, was {lock:?}"
        ));
    }
    if render_lockfile(&again) != written {
        return Err(format!(
            "a lockfile is written otherwise the second time: {written:?}"
        ));
    }
    Ok(())
}

// ── csv ─────────────────────────────────────────────────────────────

/// What the csv reader makes of any text (`csv.parse`, `csv.split_line`):
/// it ends without a panic, with an error's place inside the text; and
/// the records of a text that reads, written again with every field in
/// quotes, read back as the same records.
pub fn check_csv(text: &str) -> Result<(), String> {
    use crate::builtins::csv::{fields_of_line, record, table};
    let malformed_in = |what: &str, at: usize| match at <= text.len() && text.is_char_boundary(at) {
        true => Ok(()),
        false => Err(format!(
            "{what} at byte {at}, not in the {} bytes of the text",
            text.len()
        )),
    };
    if let Err(m) = fields_of_line(text) {
        malformed_in(m.why, m.at)?;
    }
    let _ = table(text);
    let mut records = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match record(text, at) {
            Ok((fields, next)) if next > at && next <= text.len() => {
                records.push(fields);
                at = next;
            }
            Ok((_, next)) => return Err(format!("the record at byte {at} ends at {next}")),
            Err(m) => return malformed_in(m.why, m.at),
        }
    }
    let quoted = |field: &String| format!("\"{}\"", field.replace('"', "\"\""));
    let written: String = records
        .iter()
        .map(|fields| fields.iter().map(quoted).collect::<Vec<_>>().join(",") + "\n")
        .collect();
    let mut again = Vec::new();
    let mut at = 0;
    while at < written.len() {
        let (fields, next) = record(&written, at).map_err(|m| {
            format!(
                "the records written to {written:?} do not read: {} at {}",
                m.why, m.at
            )
        })?;
        again.push(fields);
        at = next;
    }
    if again != records {
        return Err(format!(
            "the records {records:?}, written to {written:?}, read back as {again:?}"
        ));
    }
    Ok(())
}

// ── HTTP responses ──────────────────────────────────────────────────

/// Where the responses of [`check_http_response`] came from: a redirect
/// is resolved against it.
#[cfg(feature = "http")]
const RESPONSE_URL: &str = "http://example.com/a/b/c?q=1";

/// What the client of `http.get` and `http.request` makes of any
/// response a server sends. The bytes are read as a response's head (by
/// httparse, as the transport library reads one) and the body after it;
/// a text that is no head is nothing to check. The response then becomes
/// the program's `Response`, to a `GET` and to a `HEAD`, without a panic:
/// its status is the one sent, its headers are the ones sent (one entry a
/// name, in lower case; without `content-encoding` and `content-length`
/// when a gzip body was unpacked), its body the bytes sent (none to a
/// `HEAD`, or for a status that has none); only an unpacked body can fail
/// or be too large. A redirect is resolved to a URL without a fragment,
/// on the same server for a location that is a path, with no `.` or `..`
/// segment left in a path that was resolved.
#[cfg(feature = "http")]
pub fn check_http_response(bytes: &[u8]) -> Result<(), String> {
    use crate::builtins::http::{redirect, ureq_response_to_value};
    use crate::http_wire::{BODY_MAX, has_body};
    use crate::value::Value;
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut head = httparse::Response::new(&mut headers);
    let (Ok(httparse::Status::Complete(head_len)), Some(status)) = (head.parse(bytes), head.code)
    else {
        return Ok(());
    };
    let body = &bytes[head_len..];
    let sent: Vec<(String, &[u8])> = (head.headers.iter())
        .map(|header| (header.name.to_ascii_lowercase(), header.value))
        .collect();
    let response = || {
        let mut response = ureq::http::Response::builder().status(status);
        for (name, value) in &sent {
            response = response.header(name.as_str(), *value);
        }
        response
            .body(ureq::Body::builder().data(body.to_vec()))
            .ok()
    };
    // (A status or a header the http types refuse is refused by the
    // transport library too: no response reaches the program.)
    if response().is_none() {
        return Ok(());
    }
    let codings: Vec<String> = (sent.iter())
        .filter(|(name, _)| name == "content-encoding")
        .map(|(_, value)| String::from_utf8_lossy(value).into_owned())
        .collect();
    let packed = codings == ["gzip"];
    let mut names: Vec<&str> = (sent.iter())
        .map(|(name, _)| name.as_str())
        .filter(|name| !(packed && matches!(*name, "content-encoding" | "content-length")))
        .collect();
    names.sort_unstable();
    names.dedup();
    for to_head in [false, true] {
        let response = response().ok_or("a response built once is refused the second time")?;
        let value = match ureq_response_to_value(response, to_head) {
            Err(_) if packed => continue,
            Err(e) => return Err(format!("a body that is not packed failed: {e}")),
            Ok(None) if packed || body.len() > BODY_MAX => continue,
            Ok(None) => return Err(format!("a body of {} bytes is too large", body.len())),
            Ok(Some(value)) => value,
        };
        let Value::Record(record) = &value else {
            return Err(format!("the response is {value:?}"));
        };
        if record.get("status") != Some(&Value::Int(i64::from(status))) {
            return Err(format!(
                "the status {status} became {:?}",
                record.get("status")
            ));
        }
        let Some(Value::Map(map)) = record.get("headers") else {
            return Err(format!("the headers are {:?}", record.get("headers")));
        };
        let got: Vec<&str> = (map.keys())
            .map(|key| match key {
                Value::String(name, _) => Ok(&**name),
                other => Err(format!("a header name {other:?}")),
            })
            .collect::<Result<_, _>>()?;
        if got != names {
            return Err(format!("the headers {names:?} became {got:?}"));
        }
        let Some(Value::Bytes(got)) = record.get("body") else {
            return Err(format!("the body is {:?}", record.get("body")));
        };
        let expected: Option<&[u8]> = match (to_head || !has_body(status), packed) {
            (true, _) => Some(&[]),
            (false, false) => Some(body),
            (false, true) => None,
        };
        if expected.is_some_and(|expected| got.as_slice() != expected) {
            return Err(format!(
                "a body of {} bytes became {} bytes (to a HEAD: {to_head})",
                body.len(),
                got.len()
            ));
        }
    }
    let response = response().ok_or("a response built once is refused the second time")?;
    let Some((code, url)) = redirect(&response, RESPONSE_URL) else {
        return Ok(());
    };
    if !matches!(code, 301 | 302 | 303 | 307 | 308) || code != status {
        return Err(format!(
            "the response of status {status} redirected as {code}"
        ));
    }
    if url.contains('#') {
        return Err(format!("the redirect went to {url:?}, with a fragment"));
    }
    // (The first `location` header is the one followed.)
    let location = (sent.iter())
        .find(|(name, _)| name == "location")
        .map(|(_, value)| String::from_utf8_lossy(value).into_owned())
        .unwrap_or_default();
    let location = location.split('#').next().unwrap_or_default();
    if location.starts_with('/') && !location.starts_with("//") {
        let Some(path) = url.strip_prefix("http://example.com") else {
            return Err(format!(
                "the location {location:?} went to another server: {url:?}"
            ));
        };
        let path = path.split('?').next().unwrap_or_default();
        if path.split('/').any(|segment| matches!(segment, "." | "..")) {
            return Err(format!(
                "the location {location:?} went to {url:?}, dots left"
            ));
        }
    }
    Ok(())
}

// ── The editor's messages ───────────────────────────────────────────

/// What `silt lsp` makes of any bytes from the editor (stdin). The
/// transport reads the same messages, and the same that cannot be read,
/// however the bytes arrive; one that cannot be read is answered with a
/// parse error or an invalid request, with its id or `null`. The server,
/// as after the start, handles each message without a panic (a handler's
/// panic is caught and answered, but it is a fault all the same:
/// libFuzzer reports it), and answers each request it handled once, and
/// nothing else.
#[cfg(feature = "lsp")]
pub fn check_lsp_messages(bytes: &[u8]) -> Result<(), String> {
    use crate::lsp::in_memory::{read, serve};
    use lsp_server::{Message, RequestId};
    use std::collections::BTreeMap;
    let frames = read(bytes, 8 * 1024);
    let shown = |frames: &[Result<Message, (serde_json::Value, i64)>]| -> Vec<String> {
        (frames.iter())
            .map(|frame| match frame {
                Ok(message) => serde_json::to_string(message).unwrap_or_default(),
                Err(fault) => format!("{fault:?}"),
            })
            .collect()
    };
    for piece in [1, 7] {
        if shown(&read(bytes, piece)) != shown(&frames) {
            return Err(format!("the messages read {piece} bytes at a time differ"));
        }
    }
    let mut messages = Vec::new();
    for frame in frames {
        match frame {
            Ok(message) => messages.push(message),
            Err((id, code)) => {
                use serde_json::Value::{Null, Number, String};
                if !matches!(code, -32700 | -32600) || !matches!(id, Null | Number(_) | String(_)) {
                    return Err(format!(
                        "a message that cannot be read is answered {code} {id}"
                    ));
                }
            }
        }
    }
    let (sent, handled) = serve(messages);
    let mut unanswered: BTreeMap<RequestId, usize> = BTreeMap::new();
    for id in handled {
        *unanswered.entry(id).or_default() += 1;
    }
    for message in &sent {
        let Message::Response(response) = message else {
            continue;
        };
        match unanswered.get_mut(&response.id) {
            Some(count) if *count > 0 => *count -= 1,
            _ => return Err(format!("an answer to no request: {response:?}")),
        }
    }
    match unanswered.into_iter().find(|(_, count)| *count > 0) {
        Some((id, _)) => Err(format!("the request {id} has no answer")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    #[test]
    fn lexer_invariants_accept_well_formed_source() {
        let src = "let x = 1\nlet y = 2\n";
        let tokens = Lexer::new(crate::source::FileId::default(), src)
            .tokenize()
            .checked()
            .unwrap();
        check_lexer_invariants(src, &tokens).unwrap();
    }

    #[test]
    fn formatter_invariants_accept_the_formatter() {
        check_formatter_invariants("let   x=1\n\n\n-- two\nlet y=   2\n").unwrap();
        // Not a program: no input.
        check_formatter_invariants("pub fn (((\n").unwrap();
    }

    #[test]
    fn formatter_invariants_detect_a_refusal_and_a_second_pass_that_differs() {
        use crate::format::format_with;
        let file = crate::source::FileId::default();
        let source = "-- a comment\nlet x = (1 + 2)\n";
        let dropped_comment =
            |text: &str| format_with(file, text, |out| out.replace("-- a comment\n", ""));
        let err = check_formatter_invariants_of(source, dropped_comment).unwrap_err();
        assert!(err.contains("first pass"), "{err}");
        let grows =
            |text: &str| crate::format::format(file, text).map(|out| format!("-- more\n{out}"));
        let err = check_formatter_invariants_of(source, grows).unwrap_err();
        assert!(err.contains("not idempotent"), "{err}");
    }

    #[test]
    fn the_package_files_check_holds_on_a_manifest_and_a_lockfile() {
        check_package_files("[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        check_package_files("[package\nname = ").unwrap();
        let lock = "version = 1\n\n[[package]]\nname = \"b\"\nversion = \"1.0.0\"\n\
                    source = { path = \"../b\" }\n\n\
                    [[package]]\nname = \"a\"\nversion = \"0.1.0\"\n";
        check_package_files(lock).unwrap();
    }

    /// Each committed seed of the fuzz target `target`
    /// (`fuzz/corpus/<target>/*.<one of extensions>`; what a local
    /// libFuzzer run adds there has no extension) given to `check`.
    fn check_seed_bytes(
        target: &str,
        extensions: &[&str],
        check: &dyn Fn(&[u8]) -> Result<(), String>,
    ) {
        let seeds = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fuzz/corpus")
            .join(target);
        let mut read = 0;
        for seed in std::fs::read_dir(&seeds).expect("the seeds") {
            let path = seed.expect("a seed").path();
            let committed = (path.extension())
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extensions.contains(&extension));
            if !committed {
                continue;
            }
            let bytes = std::fs::read(&path).expect("a seed's bytes");
            if let Err(e) = check(&bytes) {
                panic!("{}: {e}", path.display());
            }
            read += 1;
        }
        assert!(read >= 4, "{read} seeds in {}", seeds.display());
    }

    /// [`check_seed_bytes`] for a check of text.
    fn check_seeds(target: &str, extensions: &[&str], check: fn(&str) -> Result<(), String>) {
        check_seed_bytes(target, extensions, &|bytes| {
            check(std::str::from_utf8(bytes).expect("a seed's text"))
        });
    }

    #[test]
    fn the_seeds_of_the_package_files_and_csv_targets_hold() {
        check_seeds("fuzz_package_files", &["toml", "lock"], check_package_files);
        check_seeds("fuzz_csv", &["csv"], check_csv);
    }

    #[cfg(feature = "http")]
    #[test]
    fn the_seeds_of_the_http_response_target_hold() {
        check_seed_bytes("fuzz_http_response", &["http"], &check_http_response);
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn the_seeds_of_the_lsp_messages_target_hold() {
        check_seed_bytes("fuzz_lsp_messages", &["lsp"], &check_lsp_messages);
    }

    #[cfg(feature = "http")]
    #[test]
    fn the_http_response_check_holds_on_a_body_a_gzip_body_and_a_redirect() {
        use std::io::Write;
        check_http_response(b"HTTP/1.1 200 OK\r\nA: 1\r\na: 2\r\n\r\nbody").unwrap();
        let mut packed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        packed.write_all(b"unpacked").unwrap();
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n".to_vec();
        response.extend(packed.finish().unwrap());
        check_http_response(&response).unwrap();
        check_http_response(b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\nno gzip").unwrap();
        check_http_response(b"HTTP/1.1 302 Found\r\nLocation: /x/../../y/.#f\r\n\r\n").unwrap();
        check_http_response(b"HTTP/1.1 204 No Content\r\n\r\nignored").unwrap();
        // No head: nothing to check.
        check_http_response(b"HTTP/1.1 200 OK\r\nA: 1\r\n").unwrap();
        check_http_response(b"\x00\xff").unwrap();
    }

    #[cfg(feature = "lsp")]
    #[test]
    fn the_lsp_check_answers_each_request_once() {
        let frame = |body: &str| format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///fuzz/main.silt","languageId":"silt","version":1,"text":"fn main() {\n  let x = 1\n  x\n}\n"}}}"#;
        let hover = r#"{"jsonrpc":"2.0","id":7,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///fuzz/main.silt"},"position":{"line":2,"character":2}}}"#;
        let unknown = r#"{"jsonrpc":"2.0","id":"x","method":"no/such","params":null}"#;
        let input = [open, hover, hover, unknown].map(frame).concat();
        check_lsp_messages(input.as_bytes()).unwrap();
        let read = || crate::lsp::in_memory::read(input.as_bytes(), 64);
        let (sent, handled) = crate::lsp::in_memory::serve(read().into_iter().flatten().collect());
        assert_eq!(handled.len(), 3);
        let answers = (sent.iter())
            .filter(|message| matches!(message, lsp_server::Message::Response(_)))
            .count();
        assert_eq!(answers, 3);
        // A message that cannot be read is answered by the transport, and
        // the next one is read; a truncated message ends the input.
        let input = [frame("not json"), frame(hover)].concat();
        let read = crate::lsp::in_memory::read(input.as_bytes(), 64);
        assert!(
            matches!(&read[..], [Err((serde_json::Value::Null, -32700)), Ok(_)]),
            "{read:?}"
        );
        check_lsp_messages(input.as_bytes()).unwrap();
        check_lsp_messages(b"Content-Length: 99\r\n\r\n{}").unwrap();
        check_lsp_messages(b"Content-Length: x\r\n\r\n").unwrap();
    }

    #[test]
    fn the_csv_check_holds_on_quotes_line_ends_and_errors() {
        check_csv("a,b\n1,\"x, \"\"y\"\"\"\r\n\n").unwrap();
        check_csv("\"open").unwrap();
        check_csv("a\rb").unwrap();
        check_csv("é,\"ü\"x").unwrap();
    }
}
