//! The `regex.*` builtin functions.

use std::sync::Arc;

use super::typed::{builtins, unsound};
use crate::value::Value;
use crate::vm::{Step, Vm, VmError, iterate, next};

/// What `RegexError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("RegexInvalidPattern", [Value::String(m), Value::Int(pos)]) => {
            format!("invalid regex pattern at position {pos}: {m}")
        }
        ("RegexTooBig", []) => "compiled regex exceeds size budget".to_string(),
        _ => return None,
    })
}

// ── The functions ───────────────────────────────────────────────────

fn string(s: &str) -> Value {
    Value::String(s.to_string())
}

/// The groups of a match as a list of strings, a group that took no
/// part in the match as the empty one.
fn groups(caps: &regex::Captures) -> Value {
    let groups = caps.iter().map(|m| string(m.map_or("", |m| m.as_str())));
    Value::List(Arc::new(groups.collect()))
}

builtins! {
    fn is_match(vm, pattern: &str, text: &str) -> Result<bool, VmError> {
        Ok(Vm::get_regex(&mut vm.regex_cache, pattern)?.is_match(text))
    }

    fn find(vm, pattern: &str, text: &str) -> Result<Option<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.find(text).map(|m| string(m.as_str())))
    }

    fn find_all(vm, pattern: &str, text: &str) -> Result<Vec<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.find_iter(text).map(|m| string(m.as_str())).collect())
    }

    fn split(vm, pattern: &str, text: &str) -> Result<Vec<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.split(text).map(string).collect())
    }

    fn replace(vm, pattern: &str, text: &str, replacement: &str) -> Result<String, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.replace(text, replacement).to_string())
    }

    fn replace_all(vm, pattern: &str, text: &str, replacement: &str) -> Result<String, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.replace_all(text, replacement).to_string())
    }

    // `f` is called with each match, in order, and the string it
    // returns stands for the match.
    fn replace_all_with(vm, pattern: &str, text: &str, f: &Value) -> Result<Step, VmError> {
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
            f.clone(),
            Replacing {
                text: text.to_string(),
                out: String::new(),
                last_end: 0,
            },
            |state, item, stack| {
                let (start, end) = span(item);
                stack.push(Value::String(state.text[start..end].to_string()));
            },
            |state, item, replacement| {
                let Value::String(replacement) = replacement else {
                    return Err(unsound("regex.replace_all_with", "f"));
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

    fn captures(vm, pattern: &str, text: &str) -> Result<Option<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.captures(text).map(|caps| groups(&caps)))
    }

    fn captures_all(vm, pattern: &str, text: &str) -> Result<Vec<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        Ok(re.captures_iter(text).map(|caps| groups(&caps)).collect())
    }

    fn captures_named(vm, pattern: &str, text: &str) -> Result<Option<Value>, VmError> {
        let re = Vm::get_regex(&mut vm.regex_cache, pattern)?;
        // `capture_names()` yields one entry per group, including the
        // implicit whole-match group at index 0 (whose name is `None`)
        // and any numbered-only groups (also `None`). A pattern with no
        // *named* group gives `None`.
        if re.capture_names().flatten().next().is_none() {
            return Ok(None);
        }
        let Some(caps) = re.captures(text) else {
            return Ok(None);
        };
        // Collect (name → match) pairs. Skip any named group that
        // did not participate in the match — per the spec we omit
        // it entirely rather than mapping to "".
        let named = re.capture_names().flatten().filter_map(|name| {
            let m = caps.name(name)?;
            Some((string(name), string(m.as_str())))
        });
        Ok(Some(Value::Map(Arc::new(named.collect()))))
    }
}
