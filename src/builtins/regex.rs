//! The `regex.*` builtin functions.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::common::value_kind;
use crate::typeinfo::bv;
use crate::value::Value;
use crate::vm::{BuiltinIterKind, Vm, VmError};

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
///
/// Error messages are verbatim those emitted by the pre-dedupe arms; a
/// round-36 parity test suite (`tests/meta/regex_dispatch_parity_round36_tests.rs`)
/// locks them so any accidental phrasing drift breaks loudly.
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
pub fn call_regex(vm: &mut Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
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
        "replace_all_with" => {
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
            // Materialize match spans and match texts.  Spans are re-derived
            // deterministically from (pattern, text) on resume so we don't
            // need to persist them.
            let re = Vm::get_regex(&mut vm.regex_cache, pattern)?.clone();
            let mut spans: Vec<(usize, usize)> = Vec::new();
            let mut items: Vec<Value> = Vec::new();
            for m in re.find_iter(text) {
                spans.push((m.start(), m.end()));
                items.push(Value::String(m.as_str().to_string()));
            }
            // Use iterate_builtin with ListMap semantics to collect the
            // replacement strings, with correct yield/resume handling.
            let replacements_val =
                vm.iterate_builtin(BuiltinIterKind::ListMap, items, args[2].clone(), args)?;
            let Value::List(replacements) = replacements_val else {
                return Err(VmError::new(
                    "internal VM error: regex.replace_all_with builtin iteration returned non-list"
                        .into(),
                ));
            };
            // Validate that all callback results are strings.
            for val in replacements.iter() {
                if !matches!(val, Value::String(_)) {
                    return Err(VmError::new(
                        "regex.replace_all_with callback must return a string".into(),
                    ));
                }
            }
            // Interleave text slices and replacements.
            let Value::String(text_string) = &args[1] else {
                unreachable!();
            };
            let mut result = std::string::String::new();
            let mut last_end = 0;
            for ((start, end), replacement) in spans.iter().zip(replacements.iter()) {
                result.push_str(&text_string[last_end..*start]);
                if let Value::String(s) = replacement {
                    result.push_str(s);
                }
                last_end = *end;
            }
            result.push_str(&text_string[last_end..]);
            Ok(Value::String(result))
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
