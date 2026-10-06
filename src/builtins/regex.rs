//! The `regex.*` builtin functions.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::common::value_kind;
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{Step, Vm, VmError, iterate, next};

/// Dispatch the builtin `trait Error for RegexError` method table.
/// Scaffolding lives in `super::dispatch_error_trait`; this site just
/// supplies the variant → message rendering.
pub fn call_regex_error_trait(name: &str, args: &[Value]) -> Result<Value, VmError> {
    super::dispatch_error_trait("RegexError", name, args, |tag, fields| {
        Some(match (tag, fields) {
            ("RegexInvalidPattern", [Value::String(m), Value::Int(pos)]) => {
                format!("invalid regex pattern at position {pos}: {m}")
            }
            ("RegexTooBig", []) => "compiled regex exceeds size budget".to_string(),
            _ => return None,
        })
    })
}

// ── Regex dispatch ──────────────────────────────────────────────────

/// Arity-check + typed-destructure for the common
/// `regex.<op>(pattern: String, text: String)` shape.
fn parse_regex_string_pair<'a>(
    op_name: &str,
    args: &'a [Value],
) -> Result<(&'a str, &'a str), VmError> {
    if args.len() != 2 {
        return Err(VmError::new(format!(
            "regex.{op_name} takes 2 arguments (pattern, text)"
        )));
    }
    let (Value::String(pattern), Value::String(text)) = (&args[0], &args[1]) else {
        return Err(VmError::new(format!(
            "regex.{op_name} requires String, got ({}, {})",
            value_kind(&args[0]),
            value_kind(&args[1])
        )));
    };
    Ok((pattern.as_str(), text.as_str()))
}

/// Sibling helper for the two `replace`-family arms that take a third
/// string argument (`replacement`). Same parity-lock applies.
fn parse_regex_string_triple<'a>(
    op_name: &str,
    args: &'a [Value],
) -> Result<(&'a str, &'a str, &'a str), VmError> {
    if args.len() != 3 {
        return Err(VmError::new(format!(
            "regex.{op_name} takes 3 arguments (pattern, text, replacement)"
        )));
    }
    let (Value::String(pattern), Value::String(text), Value::String(replacement)) =
        (&args[0], &args[1], &args[2])
    else {
        return Err(VmError::new(format!(
            "regex.{op_name} requires String, got ({}, {}, {})",
            value_kind(&args[0]),
            value_kind(&args[1]),
            value_kind(&args[2])
        )));
    };
    Ok((pattern.as_str(), text.as_str(), replacement.as_str()))
}

/// Dispatch `regex.<name>(args)`.
pub(crate) fn call_regex(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Step, VmError> {
    match name {
        "replace_all_with" => replace_all_with(vm, args),
        _ => regex_plain(vm, name, args).map(Step::Done),
    }
}

/// `regex.replace_all_with(pattern, text, f)`: `f` is called with each
/// match, in order, and the string it returns stands for the match.
fn replace_all_with(vm: &mut Vm, args: &[Value]) -> Result<Step, VmError> {
    if args.len() != 3 {
        return Err(VmError::new(
            "regex.replace_all_with takes 3 arguments (pattern, text, fn)".into(),
        ));
    }
    let Value::String(pattern) = &args[0] else {
        return Err(VmError::new(format!(
            "regex.replace_all_with requires String, got {}",
            value_kind(&args[0])
        )));
    };
    let Value::String(text) = &args[1] else {
        return Err(VmError::new(format!(
            "regex.replace_all_with requires String, got {}",
            value_kind(&args[1])
        )));
    };
    let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
    // The matches are the items; each carries where it ends, and the
    // state is the text built so far and the rest of the text with
    // where it starts.
    let matches = re
        .find_iter(text)
        .map(|m| {
            Value::Tuple(vec![
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
            Value::Tuple(ends) => match ends.as_slice() {
                [Value::Int(start), Value::Int(end)] => (*start as usize, *end as usize),
                _ => (0, 0),
            },
            _ => (0, 0),
        }
    }
    Ok(iterate(
        "regex.replace_all_with",
        matches,
        args[2].clone(),
        Replacing {
            text: text.clone(),
            out: String::new(),
            last_end: 0,
        },
        |state, item, stack| {
            let (start, end) = span(item);
            stack.push(Value::String(state.text[start..end].to_string()));
        },
        |state, item, replacement| {
            let Value::String(replacement) = replacement else {
                return Err(VmError::new(
                    "regex.replace_all_with callback must return a string".into(),
                ));
            };
            let (start, end) = span(&item);
            state.out.push_str(&state.text[state.last_end..start]);
            state.out.push_str(&replacement);
            state.last_end = end;
            next()
        },
        |state| {
            let mut out = std::mem::take(&mut state.out);
            out.push_str(&state.text[state.last_end..]);
            Ok(Value::String(out))
        },
    ))
}

/// The `regex` functions that call no function.
fn regex_plain(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "is_match" => {
            let (pattern, text) = parse_regex_string_pair("is_match", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::Bool(re.is_match(text)))
        }
        "find" => {
            let (pattern, text) = parse_regex_string_pair("find", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            match re.find(text) {
                Some(m) => Ok(Value::variant(
                    bv::SOME,
                    vec![Value::String(m.as_str().to_string())],
                )),
                None => Ok(Value::variant(bv::NONE, Vec::new())),
            }
        }
        "find_all" => {
            let (pattern, text) = parse_regex_string_pair("find_all", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let matches: Vec<Value> = re
                .find_iter(text)
                .map(|m| Value::String(m.as_str().to_string()))
                .collect();
            Ok(Value::List(Arc::new(matches)))
        }
        "split" => {
            let (pattern, text) = parse_regex_string_pair("split", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let parts: Vec<Value> = re
                .split(text)
                .map(|s| Value::String(s.to_string()))
                .collect();
            Ok(Value::List(Arc::new(parts)))
        }
        "replace" => {
            let (pattern, text, replacement) = parse_regex_string_triple("replace", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::String(re.replace(text, replacement).to_string()))
        }
        "replace_all" => {
            let (pattern, text, replacement) = parse_regex_string_triple("replace_all", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            Ok(Value::String(re.replace_all(text, replacement).to_string()))
        }
        "captures" => {
            let (pattern, text) = parse_regex_string_pair("captures", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            match re.captures(text) {
                Some(caps) => {
                    let groups: Vec<Value> = caps
                        .iter()
                        .map(|m| match m {
                            Some(m) => Value::String(m.as_str().to_string()),
                            None => Value::String(std::string::String::new()),
                        })
                        .collect();
                    Ok(Value::variant(
                        bv::SOME,
                        vec![Value::List(Arc::new(groups))],
                    ))
                }
                None => Ok(Value::variant(bv::NONE, Vec::new())),
            }
        }
        "captures_all" => {
            let (pattern, text) = parse_regex_string_pair("captures_all", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            let all_captures: Vec<Value> = re
                .captures_iter(text)
                .map(|caps| {
                    let groups: Vec<Value> = caps
                        .iter()
                        .map(|m| match m {
                            Some(m) => Value::String(m.as_str().to_string()),
                            None => Value::String(std::string::String::new()),
                        })
                        .collect();
                    Value::List(Arc::new(groups))
                })
                .collect();
            Ok(Value::List(Arc::new(all_captures)))
        }
        "captures_named" => {
            let (pattern, text) = parse_regex_string_pair("captures_named", args)?;
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
            // `capture_names()` yields one entry per group, including the
            // implicit whole-match group at index 0 (whose name is
            // `None`) and any numbered-only groups (also `None`). We
            // count only the *named* entries to decide whether the
            // pattern is "nameless" — if so, the contract says `None`.
            let named_count = re.capture_names().flatten().count();
            if named_count == 0 {
                return Ok(Value::variant(bv::NONE, Vec::new()));
            }
            let Some(caps) = re.captures(text) else {
                return Ok(Value::variant(bv::NONE, Vec::new()));
            };
            // Collect (name → match) pairs. Skip any named group that
            // did not participate in the match — per the spec we omit
            // it entirely rather than mapping to "".
            let mut out: BTreeMap<Value, Value> = BTreeMap::new();
            for name in re.capture_names().flatten() {
                if let Some(m) = caps.name(name) {
                    out.insert(
                        Value::String(name.to_string()),
                        Value::String(m.as_str().to_string()),
                    );
                }
            }
            Ok(Value::variant(bv::SOME, vec![Value::Map(Arc::new(out))]))
        }
        _ => Err(VmError::new(format!("unknown regex function: {name}"))),
    }
}
