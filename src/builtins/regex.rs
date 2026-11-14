//! The `regex.*` builtin functions.

use regex_automata::PatternID;
use regex_automata::meta::{BuildError, Regex};
use regex_automata::util::captures::Captures;

use std::ops::Range;
use std::sync::Arc;

use super::common::{ok, too_long_text};
use super::typed::{self, Text, builtins, unsound};
use crate::typeinfo::bv;
use crate::value::{MAX_TEXT_LEN, Value};
use crate::vm::{Step, VmError, iterate, next};

/// How `RegexError` shows the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("InvalidPattern", [Value::String(m, _), Value::Int(pos)]) => {
            format!("invalid regex pattern at position {pos}: {m}")
        }
        ("TooBig", []) => "compiled regex exceeds size budget".to_string(),
        _ => return None,
    })
}

/// A `Regex` value (`regex.compile`): the pattern as it was written,
/// and the regex it compiled to. Two of one pattern are equal.
pub struct Compiled {
    pub(crate) pattern: Box<str>,
    pub(crate) re: Arc<Regex>,
}

// ── The functions ───────────────────────────────────────────────────

fn string(s: &str) -> Value {
    Value::string(s)
}

/// The groups of a match in `text` as a list of strings, a group that
/// took no part in the match as the empty one.
fn groups(text: &str, caps: &Captures) -> Value {
    let groups = caps.iter().map(|m| string(m.map_or("", |m| &text[m])));
    Value::list(groups.collect())
}

fn option(found: Option<Value>) -> Value {
    match found {
        Some(value) => Value::variant(bv::SOME, [value]),
        None => Value::variant(bv::NONE, []),
    }
}

/// What is wrong with a pattern, and the character of the pattern
/// where it is wrong, counted from 0 as `string.slice` counts: where the
/// fault begins, or the pattern's length when the fault is that the
/// pattern ends too soon (`a\`, `\x`). The pattern is parsed again
/// with the parser the `regex` crate uses, whose error says both;
/// `report`, the `regex` crate's text, stands only where that parser
/// finds no fault, with the place 0.
fn syntax_error(pattern: &str, report: &str) -> (String, usize) {
    use regex_syntax::ast::ErrorKind;
    let chars = |offset: usize| pattern[..offset].chars().count();
    match regex_syntax::ParserBuilder::new()
        .nest_limit(250)
        .build()
        .parse(pattern)
    {
        Err(regex_syntax::Error::Parse(error)) => {
            let at = match error.kind() {
                ErrorKind::EscapeUnexpectedEof
                | ErrorKind::FlagUnexpectedEof
                | ErrorKind::GroupNameUnexpectedEof
                | ErrorKind::SpecialWordOrRepetitionUnexpectedEof => pattern.chars().count(),
                _ => chars(error.span().start.offset),
            };
            (error.kind().to_string(), at)
        }
        Err(regex_syntax::Error::Translate(error)) => {
            (error.kind().to_string(), chars(error.span().start.offset))
        }
        _ => (report.to_string(), 0),
    }
}

/// `Err(RegexError)` of the pattern `pattern`, which did not compile:
/// `TooBig` past the size limit, else what the parser says is wrong
/// with it (as `regex::Error` sorts the two).
fn pattern_err(pattern: &str, error: &BuildError) -> Value {
    let variant = if error.size_limit().is_some() {
        Value::variant(bv::REGEX_TOO_BIG, [])
    } else {
        let report = match error.syntax_error() {
            Some(syntax) => syntax.to_string(),
            None => error.to_string(),
        };
        let (what, at) = syntax_error(pattern, &report);
        Value::variant(
            bv::REGEX_INVALID_PATTERN,
            [Value::string(what), Value::Int(at as i64)],
        )
    };
    Value::variant(bv::ERR, [variant])
}

/// `text` with its first `most` matches of `re` replaced by
/// `replacement` (its `$1` and `$name` expanded), as `Regex::replacen`
/// does, for `name`: the text grows match by match, and stops where it
/// would be longer than a string may be.
fn replaced(
    name: &str,
    re: &Regex,
    text: &str,
    replacement: &str,
    most: usize,
) -> Result<Value, VmError> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text).take(most) {
        let whole = caps.get_group(0).map_or(0..0, |m| m.range());
        out.push_str(&text[last..whole.start]);
        caps.interpolate_string_into(text, replacement, &mut out);
        last = whole.end;
        if out.len() > MAX_TEXT_LEN {
            return Err(too_long_text(name, out.len()));
        }
    }
    out.push_str(&text[last..]);
    if out.len() > MAX_TEXT_LEN {
        return Err(too_long_text(name, out.len()));
    }
    Ok(string(&out))
}

/// The pieces of `text` between the matches of `re`, cut as
/// `string.split` cuts: a match of nothing at the start or the end of
/// the text cuts nothing there, so it gives no empty piece (`""` cuts
/// `"abc"` into its characters, and empty text into none).
fn pieces<'t>(re: &Regex, text: &'t str) -> Vec<&'t str> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut matched = false;
    for m in re.find_iter(text) {
        matched = true;
        if m.is_empty() && (m.start() == 0 || m.start() == text.len()) {
            continue;
        }
        pieces.push(&text[start..m.start()]);
        start = m.end();
    }
    // (Empty text that a match of nothing cut is no piece.)
    if !(text.is_empty() && matched) {
        pieces.push(&text[start..]);
    }
    pieces
}

/// Where each of `matches`, byte ranges of `text` in order and not
/// overlapping, is: `(start, end)` in characters, as `string.slice` and
/// `string.index_of` count, the end not in the match. In ASCII text a
/// byte offset is the character offset; in other text the characters
/// are counted from one match to the next, so the text is walked once.
fn positions(text: Text, matches: impl Iterator<Item = Range<usize>>) -> Vec<Value> {
    let bytes = text.text.as_bytes();
    // (A character starts at each byte that is not a continuation byte,
    // 0b10xx_xxxx.)
    let (mut byte, mut chars) = (0, 0);
    let mut at = |offset: usize| -> i64 {
        if text.ascii {
            return offset as i64;
        }
        chars += bytes[byte..offset]
            .iter()
            .filter(|b| (**b as i8) >= -0x40)
            .count();
        byte = offset;
        chars as i64
    };
    matches
        .map(|m| {
            let start = at(m.start);
            let end = at(m.end);
            Value::tuple(vec![Value::Int(start), Value::Int(end)])
        })
        .collect()
}

// `compile` checks a pattern once; every other function takes the text
// first and the compiled pattern second, and gives its value bare.
builtins! {
    fn compile(vm, pattern: &str) -> Value {
        match vm.regex(pattern) {
            Ok(re) => ok(Value::Regex(Arc::new(Compiled {
                pattern: pattern.into(),
                re,
            }))),
            Err(error) => pattern_err(pattern, &error),
        }
    }

    // Every character that means something in a pattern gets a `\`,
    // so the pattern matches the text as it is.
    fn escape(text: &str) -> String {
        regex_syntax::escape(text)
    }

    fn is_match(text: &str, re: typed::Regex) -> bool {
        re.re.is_match(text)
    }

    fn find(text: &str, re: typed::Regex) -> Value {
        option(re.re.find(text).map(|m| string(&text[m.range()])))
    }

    fn find_all(text: &str, re: typed::Regex) -> Value {
        Value::list(re.re.find_iter(text).map(|m| string(&text[m.range()])).collect())
    }

    fn find_at(text: Text, re: typed::Regex) -> Value {
        let first = re.re.find(text.text).map(|m| m.range());
        option(positions(text, first.into_iter()).pop())
    }

    fn find_all_at(text: Text, re: typed::Regex) -> Value {
        Value::list(positions(text, re.re.find_iter(text.text).map(|m| m.range())))
    }

    fn split(text: &str, re: typed::Regex) -> Value {
        Value::list(pieces(&re.re, text).into_iter().map(string).collect())
    }

    fn replace_first(text: &str, re: typed::Regex, replacement: &str) -> Result<Value, VmError> {
        replaced("regex.replace_first", &re.re, text, replacement, 1)
    }

    fn replace(text: &str, re: typed::Regex, replacement: &str) -> Result<Value, VmError> {
        replaced("regex.replace", &re.re, text, replacement, usize::MAX)
    }

    // `f` is called with each match, in order, and the string it
    // returns stands for the match.
    fn replace_with(text: &str, re: typed::Regex, f: &Value) -> Step {
        // The matches are the items; each carries where it ends, and the
        // state is the text built so far and the rest of the text with
        // where it starts.
        let matches: Vec<Value> = re
            .re
            .find_iter(text)
            .map(|m| {
                Value::tuple(vec![
                    Value::Int(m.start() as i64),
                    Value::Int(m.end() as i64),
                ])
            })
            .collect();
        struct Replacing {
            text: String,
            out: String,
            last_end: usize,
        }
        fn span(item: &Value) -> (usize, usize) {
            match item {
                Value::Tuple(ends) => match &ends[..] {
                    [Value::Int(start), Value::Int(end)] => (*start as usize, *end as usize),
                    _ => (0, 0),
                },
                _ => (0, 0),
            }
        }
        iterate(
            "regex.replace_with",
            matches,
            f.clone(),
            Replacing {
                text: text.to_string(),
                out: String::new(),
                last_end: 0,
            },
            |state, item, stack| {
                let (start, end) = span(item);
                stack.push(Value::string(&state.text[start..end]));
            },
            |state, item, replacement| {
                let Value::String(replacement, _) = replacement else {
                    return Err(unsound("regex.replace_with", "f"));
                };
                let (start, end) = span(&item);
                state.out.push_str(&state.text[state.last_end..start]);
                state.out.push_str(&replacement);
                state.last_end = end;
                if state.out.len() > MAX_TEXT_LEN {
                    return Err(too_long_text("regex.replace_with", state.out.len()));
                }
                next()
            },
            |state| {
                let mut out = std::mem::take(&mut state.out);
                out.push_str(&state.text[state.last_end..]);
                if out.len() > MAX_TEXT_LEN {
                    return Err(too_long_text("regex.replace_with", out.len()));
                }
                Ok(Value::string(out))
            },
        )
    }

    fn captures(text: &str, re: typed::Regex) -> Value {
        let re = &re.re;
        let mut caps = re.create_captures();
        re.captures(text, &mut caps);
        option(caps.is_match().then(|| groups(text, &caps)))
    }

    fn captures_all(text: &str, re: typed::Regex) -> Value {
        Value::list(re.re.captures_iter(text).map(|caps| groups(text, &caps)).collect())
    }

    fn captures_named(text: &str, re: typed::Regex) -> Value {
        let re = &re.re;
        // The names yield one entry per group, including the
        // implicit whole-match group at index 0 (whose name is `None`)
        // and any numbered-only groups (also `None`). A pattern with no
        // *named* group gives `None`.
        let names = || re.group_info().pattern_names(PatternID::ZERO).flatten();
        if names().next().is_none() {
            return option(None);
        }
        let mut caps = re.create_captures();
        re.captures(text, &mut caps);
        if !caps.is_match() {
            return option(None);
        }
        // Collect (name → match) pairs. Skip any named group that
        // did not participate in the match — per the spec we omit
        // it entirely rather than mapping to "".
        let named = names().filter_map(|name| {
            let m = caps.get_group_by_name(name)?;
            Some((string(name), string(&text[m])))
        });
        option(Some(Value::map(named)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The positions of every match of `pattern` in `text`, with the
    /// text marked as ASCII or not.
    fn at(pattern: &str, text: &str, ascii: bool) -> Vec<(i64, i64)> {
        let re = Regex::new(pattern).expect("the pattern compiles");
        let found = positions(Text { text, ascii }, re.find_iter(text).map(|m| m.range()));
        found
            .iter()
            .map(|pair| match pair {
                Value::Tuple(ends) => match &ends[..] {
                    [Value::Int(start), Value::Int(end)] => (*start, *end),
                    _ => panic!("not a pair of Ints: {pair:?}"),
                },
                _ => panic!("not a tuple: {pair:?}"),
            })
            .collect()
    }

    /// A position counts characters, as `string.slice` does: the bytes
    /// of a character outside ASCII before a match count once.
    #[test]
    fn a_match_position_counts_characters() {
        assert_eq!(at("ab", "éé ab", false), [(3, 5)]);
        assert_eq!(at("b+", "日本 bb 語 b", false), [(3, 5), (8, 9)]);
        assert_eq!(at("é+", "aéé", false), [(1, 3)]);
        assert_eq!(at("x", "日本", false), []);
    }

    /// A match of nothing has its start as its end, at each character
    /// boundary, never inside a character.
    #[test]
    fn an_empty_match_is_a_position_between_characters() {
        assert_eq!(at("", "aé", false), [(0, 0), (1, 1), (2, 2)]);
        assert_eq!(at("", "", true), [(0, 0)]);
    }

    /// The ASCII path (byte offsets taken as they are) gives the numbers
    /// the counting path gives, on text that is ASCII.
    #[test]
    fn the_ascii_path_gives_what_counting_gives() {
        for (pattern, text) in [
            ("ab", "cab ab"),
            (r"\bab", "cab ab"),
            ("", "abc"),
            (r"\d+", "a1 b22 c333"),
            ("z", "abc"),
        ] {
            assert_eq!(
                at(pattern, text, true),
                at(pattern, text, false),
                "{pattern} in {text}"
            );
        }
        assert_eq!(at(r"\bab", "cab ab", true), [(4, 6)]);
    }
}
