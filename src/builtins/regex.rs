//! The `regex.*` builtin functions.

use super::common::{ok, too_long_text};
use super::typed::{builtins, unsound};
use crate::typeinfo::bv;
use crate::value::{MAX_COLLECTION_LEN, Value};
use crate::vm::{Step, Vm, VmError, iterate, next};

/// What `RegexError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("InvalidPattern", [Value::String(m), Value::Int(pos)]) => {
            format!("invalid regex pattern at position {pos}: {m}")
        }
        ("TooBig", []) => "compiled regex exceeds size budget".to_string(),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

// ── The functions ───────────────────────────────────────────────────

fn string(s: &str) -> Value {
    Value::String(s.into())
}

/// The groups of a match as a list of strings, a group that took no
/// part in the match as the empty one.
fn groups(caps: &regex::Captures) -> Value {
    let groups = caps.iter().map(|m| string(m.map_or("", |m| m.as_str())));
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

/// `Err(RegexError)` of the pattern `pattern`, which did not compile.
fn pattern_err(pattern: &str, error: &regex::Error) -> Value {
    let variant = match error {
        regex::Error::Syntax(report) => {
            let (what, at) = syntax_error(pattern, report);
            Value::variant(
                bv::REGEX_INVALID_PATTERN,
                [Value::String(what.into()), Value::Int(at as i64)],
            )
        }
        regex::Error::CompiledTooBig(_) => Value::variant(bv::REGEX_TOO_BIG, []),
        other => Value::variant(bv::REGEX_OTHER, [Value::String(other.to_string().into())]),
    };
    Value::variant(bv::ERR, [variant])
}

/// What a function of the pattern gives: `Ok` of what `found` makes of
/// the compiled pattern, or `Err(RegexError)` if it does not compile.
/// `text` with its first `most` matches of `re` replaced by
/// `replacement` (its `$1` and `$name` expanded), as `Regex::replacen`
/// does, for `name`: the text grows match by match, and stops where it
/// would be longer than a string may be.
fn replaced(
    name: &str,
    re: &regex::Regex,
    text: &str,
    replacement: &str,
    most: usize,
) -> Result<Value, VmError> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in re.captures_iter(text).take(most) {
        let whole = caps.get(0).map_or(0..0, |m| m.range());
        out.push_str(&text[last..whole.start]);
        caps.expand(replacement, &mut out);
        last = whole.end;
        if out.len() > MAX_COLLECTION_LEN {
            return Err(too_long_text(name, out.len()));
        }
    }
    out.push_str(&text[last..]);
    if out.len() > MAX_COLLECTION_LEN {
        return Err(too_long_text(name, out.len()));
    }
    Ok(string(&out))
}

fn with(vm: &mut Vm, pattern: &str, found: impl FnOnce(&regex::Regex) -> Value) -> Value {
    match Vm::get_regex(&mut vm.regex_cache, pattern) {
        Ok(re) => ok(found(re)),
        Err(error) => pattern_err(pattern, &error),
    }
}

// Every function takes the text first and the pattern second, and
// returns a `Result`: a pattern is text like any other, and one that
// is no pattern is an error value.
builtins! {
    fn is_match(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| Value::Bool(re.is_match(text)))
    }

    fn find(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| option(re.find(text).map(|m| string(m.as_str()))))
    }

    fn find_all(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| {
            Value::list(re.find_iter(text).map(|m| string(m.as_str())).collect())
        })
    }

    fn split(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| Value::list(re.split(text).map(string).collect()))
    }

    fn replace(vm, text: &str, pattern: &str, replacement: &str) -> Result<Value, VmError> {
        match Vm::get_regex(&mut vm.regex_cache, pattern) {
            Ok(re) => Ok(ok(replaced("regex.replace", re, text, replacement, 1)?)),
            Err(error) => Ok(pattern_err(pattern, &error)),
        }
    }

    fn replace_all(vm, text: &str, pattern: &str, replacement: &str) -> Result<Value, VmError> {
        match Vm::get_regex(&mut vm.regex_cache, pattern) {
            Ok(re) => Ok(ok(replaced("regex.replace_all", re, text, replacement, usize::MAX)?)),
            Err(error) => Ok(pattern_err(pattern, &error)),
        }
    }

    // `f` is called with each match, in order, and the string it
    // returns stands for the match.
    fn replace_all_with(vm, text: &str, pattern: &str, f: &Value) -> Step {
        let re = match Vm::get_regex(&mut vm.regex_cache, pattern) {
            Ok(re) => re,
            Err(error) => return Step::Done(pattern_err(pattern, &error)),
        };
        // The matches are the items; each carries where it ends, and the
        // state is the text built so far and the rest of the text with
        // where it starts.
        let matches: Vec<Value> = re
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
            "regex.replace_all_with",
            matches,
            f.clone(),
            Replacing {
                text: text.to_string(),
                out: String::new(),
                last_end: 0,
            },
            |state, item, stack| {
                let (start, end) = span(item);
                stack.push(Value::String(state.text[start..end].into()));
            },
            |state, item, replacement| {
                let Value::String(replacement) = replacement else {
                    return Err(unsound("regex.replace_all_with", "f"));
                };
                let (start, end) = span(&item);
                state.out.push_str(&state.text[state.last_end..start]);
                state.out.push_str(&replacement);
                state.last_end = end;
                if state.out.len() > MAX_COLLECTION_LEN {
                    return Err(too_long_text("regex.replace_all_with", state.out.len()));
                }
                next()
            },
            |state| {
                let mut out = std::mem::take(&mut state.out);
                out.push_str(&state.text[state.last_end..]);
                if out.len() > MAX_COLLECTION_LEN {
                    return Err(too_long_text("regex.replace_all_with", out.len()));
                }
                Ok(ok(Value::String(out.into())))
            },
        )
    }

    fn captures(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| option(re.captures(text).map(|caps| groups(&caps))))
    }

    fn captures_all(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| {
            Value::list(re.captures_iter(text).map(|caps| groups(&caps)).collect())
        })
    }

    fn captures_named(vm, text: &str, pattern: &str) -> Value {
        with(vm, pattern, |re| {
            // `capture_names()` yields one entry per group, including the
            // implicit whole-match group at index 0 (whose name is `None`)
            // and any numbered-only groups (also `None`). A pattern with no
            // *named* group gives `None`.
            if re.capture_names().flatten().next().is_none() {
                return option(None);
            }
            let Some(caps) = re.captures(text) else {
                return option(None);
            };
            // Collect (name → match) pairs. Skip any named group that
            // did not participate in the match — per the spec we omit
            // it entirely rather than mapping to "".
            let named = re.capture_names().flatten().filter_map(|name| {
                let m = caps.name(name)?;
                Some((string(name), string(m.as_str())))
            });
            option(Some(Value::map(named)))
        })
    }
}
