//! String builtin functions (`string.*`).

use std::sync::Arc;

use crate::value::{MAX_RANGE_MATERIALIZE, Value, checked_range_len};
use crate::vm::{Vm, VmError};

/// Dispatch `string.<name>(args)`.
pub fn call(vm: &Vm, name: &str, args: &[Value]) -> Result<Value, VmError> {
    match name {
        "from" => {
            if args.len() != 1 {
                return Err(VmError::new("string.from takes 1 argument".into()));
            }
            Ok(Value::String(vm.display_value(&args[0])))
        }
        "split" => {
            if args.len() != 2 {
                return Err(VmError::new("string.split takes 2 arguments".into()));
            }
            let (Value::String(s), Value::String(sep)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.split requires strings".into()));
            };
            let parts: Vec<Value> = s
                .split(sep.as_str())
                .map(|p| Value::String(p.to_string()))
                .collect();
            Ok(Value::List(Arc::new(parts)))
        }
        "trim" => {
            if args.len() != 1 {
                return Err(VmError::new("string.trim takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.trim requires a string".into()));
            };
            Ok(Value::String(s.trim().to_string()))
        }
        "trim_start" => {
            if args.len() != 1 {
                return Err(VmError::new("string.trim_start takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.trim_start requires a string".into()));
            };
            Ok(Value::String(s.trim_start().to_string()))
        }
        "trim_end" => {
            if args.len() != 1 {
                return Err(VmError::new("string.trim_end takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.trim_end requires a string".into()));
            };
            Ok(Value::String(s.trim_end().to_string()))
        }
        "contains" => {
            if args.len() != 2 {
                return Err(VmError::new("string.contains takes 2 arguments".into()));
            }
            let (Value::String(s), Value::String(sub)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.contains requires strings".into()));
            };
            Ok(Value::Bool(s.contains(sub.as_str())))
        }
        "replace" => {
            if args.len() != 3 {
                return Err(VmError::new("string.replace takes 3 arguments".into()));
            }
            let (Value::String(s), Value::String(from), Value::String(to)) =
                (&args[0], &args[1], &args[2])
            else {
                return Err(VmError::new("string.replace requires strings".into()));
            };
            // Cap the worst-case result length. Without this, a call like
            // `s.replace("", long_to)` inserts `to` at every byte boundary,
            // producing `(|s| + 1) * |to| + |s|` bytes and can trivially
            // blow out RAM. Sibling builtins (`string.repeat`,
            // `string.pad_left`, `string.pad_right`) cap at
            // `MAX_RANGE_MATERIALIZE`; mirror that exactly.
            let s_len = s.len() as u128;
            let from_len = from.len() as u128;
            let to_len = to.len() as u128;
            let result_len: u128 = if from_len == 0 {
                // Rust inserts `to` between every byte (including both ends):
                // result = (|s| + 1) * |to| + |s|.
                s_len
                    .saturating_add(1)
                    .saturating_mul(to_len)
                    .saturating_add(s_len)
            } else {
                // Count occurrences to compute the exact result length:
                // result = |s| + occurrences * (|to| - |from|).
                let occurrences = s.matches(from.as_str()).count() as u128;
                if to_len >= from_len {
                    s_len.saturating_add(occurrences.saturating_mul(to_len - from_len))
                } else {
                    let shrink = occurrences.saturating_mul(from_len - to_len);
                    s_len.saturating_sub(shrink)
                }
            };
            if result_len > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "string.replace: result would exceed maximum string size ({} bytes > {} limit)",
                    result_len, MAX_RANGE_MATERIALIZE
                )));
            }
            Ok(Value::String(s.replace(from.as_str(), to.as_str())))
        }
        "join" => {
            if args.len() != 2 {
                return Err(VmError::new("string.join takes 2 arguments".into()));
            }
            let Value::String(sep) = &args[1] else {
                return Err(VmError::new(
                    "string.join separator must be a string".into(),
                ));
            };
            let strs: Vec<String> = match &args[0] {
                Value::List(xs) => xs.iter().map(|v| v.to_string()).collect(),
                Value::Range(lo, hi) => {
                    checked_range_len(*lo, *hi).map_err(VmError::new)?;
                    (*lo..=*hi).map(|i| i.to_string()).collect()
                }
                _ => return Err(VmError::new("string.join requires a list or range".into())),
            };
            Ok(Value::String(strs.join(sep.as_str())))
        }
        "length" => {
            if args.len() != 1 {
                return Err(VmError::new("string.length takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.length requires a string".into()));
            };
            Ok(Value::Int(s.chars().count() as i64))
        }
        "byte_length" => {
            if args.len() != 1 {
                return Err(VmError::new("string.byte_length takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.byte_length requires a string".into()));
            };
            Ok(Value::Int(s.len() as i64))
        }
        "to_upper" => {
            if args.len() != 1 {
                return Err(VmError::new("string.to_upper takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.to_upper requires a string".into()));
            };
            Ok(Value::String(s.to_uppercase()))
        }
        "to_lower" => {
            if args.len() != 1 {
                return Err(VmError::new("string.to_lower takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.to_lower requires a string".into()));
            };
            Ok(Value::String(s.to_lowercase()))
        }
        "starts_with" => {
            if args.len() != 2 {
                return Err(VmError::new("string.starts_with takes 2 arguments".into()));
            }
            let (Value::String(s), Value::String(prefix)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.starts_with requires strings".into()));
            };
            Ok(Value::Bool(s.starts_with(prefix.as_str())))
        }
        "ends_with" => {
            if args.len() != 2 {
                return Err(VmError::new("string.ends_with takes 2 arguments".into()));
            }
            let (Value::String(s), Value::String(suffix)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.ends_with requires strings".into()));
            };
            Ok(Value::Bool(s.ends_with(suffix.as_str())))
        }
        "chars" => {
            if args.len() != 1 {
                return Err(VmError::new("string.chars takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.chars requires a string".into()));
            };
            let chars: Vec<Value> = s.chars().map(|c| Value::String(c.to_string())).collect();
            Ok(Value::List(Arc::new(chars)))
        }
        "repeat" => {
            if args.len() != 2 {
                return Err(VmError::new("string.repeat takes 2 arguments".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.repeat requires a string".into()));
            };
            let Value::Int(n) = &args[1] else {
                return Err(VmError::new("string.repeat requires an int".into()));
            };
            let n_val = *n;
            if n_val < 0 {
                return Err(VmError::new(format!(
                    "string.repeat: negative count {n_val}"
                )));
            }
            let result_len = (n_val as u128) * (s.len() as u128);
            if result_len > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "string.repeat: result would exceed maximum string size ({} bytes > {} limit)",
                    result_len, MAX_RANGE_MATERIALIZE
                )));
            }
            Ok(Value::String(s.repeat(n_val as usize)))
        }
        "index_of" => {
            if args.len() != 2 {
                return Err(VmError::new("string.index_of takes 2 arguments".into()));
            }
            let (Value::String(s), Value::String(needle)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.index_of requires strings".into()));
            };
            match s.find(needle.as_str()) {
                Some(byte_pos) => {
                    let char_pos = s[..byte_pos].chars().count();
                    Ok(Value::Variant(
                        "Some".into(),
                        vec![Value::Int(char_pos as i64)],
                    ))
                }
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
        }
        "last_index_of" => {
            if args.len() != 2 {
                return Err(VmError::new(
                    "string.last_index_of takes 2 arguments".into(),
                ));
            }
            let (Value::String(s), Value::String(needle)) = (&args[0], &args[1]) else {
                return Err(VmError::new("string.last_index_of requires strings".into()));
            };
            match s.rfind(needle.as_str()) {
                Some(byte_pos) => {
                    // Match string.index_of: return a CHARACTER index (not byte).
                    let char_pos = s[..byte_pos].chars().count();
                    Ok(Value::Variant(
                        "Some".into(),
                        vec![Value::Int(char_pos as i64)],
                    ))
                }
                None => Ok(Value::Variant("None".into(), Vec::new())),
            }
        }
        "split_at" => {
            if args.len() != 2 {
                return Err(VmError::new("string.split_at takes 2 arguments".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.split_at requires a string".into()));
            };
            let Value::Int(idx) = &args[1] else {
                return Err(VmError::new("string.split_at requires an int".into()));
            };
            let idx_val = *idx;
            if idx_val < 0 {
                return Err(VmError::new(format!(
                    "string.split_at: negative index {idx_val}"
                )));
            }
            // Character indexing (consistent with string.index_of / string.slice).
            // With char indices every valid index is a UTF-8 boundary; we still
            // keep the boundary check defensively so the error surface stays
            // useful if anyone later migrates this to byte offsets.
            let mut char_byte_boundary: Option<usize> = None;
            let mut seen = 0i64;
            if idx_val == 0 {
                char_byte_boundary = Some(0);
            } else {
                for (byte_pos, _) in s.char_indices() {
                    if seen == idx_val {
                        char_byte_boundary = Some(byte_pos);
                        break;
                    }
                    seen += 1;
                }
                if char_byte_boundary.is_none() && seen == idx_val {
                    // idx_val == char count: split at end-of-string.
                    char_byte_boundary = Some(s.len());
                }
            }
            let Some(boundary) = char_byte_boundary else {
                return Err(VmError::new(format!(
                    "string.split_at: index {idx_val} out of bounds (length {})",
                    s.chars().count()
                )));
            };
            if !s.is_char_boundary(boundary) {
                return Err(VmError::new(format!(
                    "string.split_at: index {idx_val} is not on a UTF-8 character boundary"
                )));
            }
            let (left, right) = s.split_at(boundary);
            Ok(Value::Tuple(vec![
                Value::String(left.to_string()),
                Value::String(right.to_string()),
            ]))
        }
        "lines" => {
            if args.len() != 1 {
                return Err(VmError::new("string.lines takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.lines requires a string".into()));
            };
            // Split on '\n' only. A trailing '\n' must NOT produce an empty
            // final element (matches user expectation: "a\nb\n".lines() ==
            // ["a", "b"]). Also strip a trailing '\r' from each line to
            // normalise \r\n line endings. Empty input yields [] (matches
            // Rust's str::lines and Python's str.splitlines).
            if s.is_empty() {
                return Ok(Value::List(Arc::new(Vec::new())));
            }
            let mut lines: Vec<Value> = Vec::new();
            let mut iter = s.split('\n').peekable();
            while let Some(part) = iter.next() {
                // If this is the final empty segment that comes from a
                // trailing '\n', drop it.
                if part.is_empty() && iter.peek().is_none() && s.ends_with('\n') {
                    break;
                }
                let trimmed = part.strip_suffix('\r').unwrap_or(part);
                lines.push(Value::String(trimmed.to_string()));
            }
            Ok(Value::List(Arc::new(lines)))
        }
        "starts_with_at" => {
            if args.len() != 3 {
                return Err(VmError::new(
                    "string.starts_with_at takes 3 arguments".into(),
                ));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "string.starts_with_at requires a string".into(),
                ));
            };
            let Value::Int(offset) = &args[1] else {
                return Err(VmError::new(
                    "string.starts_with_at requires an int offset".into(),
                ));
            };
            let Value::String(prefix) = &args[2] else {
                return Err(VmError::new(
                    "string.starts_with_at requires a string prefix".into(),
                ));
            };
            // Predicate: out-of-range offset returns false rather than
            // panicking (contract with string.starts_with is a boolean).
            let offset_val = *offset;
            if offset_val < 0 {
                return Ok(Value::Bool(false));
            }
            // Character indexing, consistent with string.index_of /
            // string.slice. Convert the char offset to a byte offset; if the
            // char offset is past the end of the string, return false.
            let offset_usize = offset_val as usize;
            let mut byte_offset: Option<usize> = None;
            if offset_usize == 0 {
                byte_offset = Some(0);
            } else {
                let mut seen = 0usize;
                for (bpos, _) in s.char_indices() {
                    if seen == offset_usize {
                        byte_offset = Some(bpos);
                        break;
                    }
                    seen += 1;
                }
                if byte_offset.is_none() && seen == offset_usize {
                    byte_offset = Some(s.len());
                }
            }
            let Some(bpos) = byte_offset else {
                return Ok(Value::Bool(false));
            };
            Ok(Value::Bool(s[bpos..].starts_with(prefix.as_str())))
        }
        "slice" => {
            if args.len() != 3 {
                return Err(VmError::new("string.slice takes 3 arguments".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("first arg must be string".into()));
            };
            let Value::Int(start) = &args[1] else {
                return Err(VmError::new("second arg must be int".into()));
            };
            let Value::Int(end) = &args[2] else {
                return Err(VmError::new("third arg must be int".into()));
            };
            let start_val = *start;
            if start_val < 0 {
                return Err(VmError::new(format!(
                    "string.slice: negative index {start_val}"
                )));
            }
            let end_val = *end;
            if end_val < 0 {
                return Err(VmError::new(format!(
                    "string.slice: negative index {end_val}"
                )));
            }
            let chars: Vec<char> = s.chars().collect();
            let start = (start_val as usize).min(chars.len());
            let end = (end_val as usize).min(chars.len());
            if start > end {
                Ok(Value::String(String::new()))
            } else {
                Ok(Value::String(chars[start..end].iter().collect()))
            }
        }
        "pad_left" => {
            if args.len() != 3 {
                return Err(VmError::new("string.pad_left takes 3 arguments".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("first arg must be string".into()));
            };
            let Value::Int(width) = &args[1] else {
                return Err(VmError::new("second arg must be int".into()));
            };
            let Value::String(pad) = &args[2] else {
                return Err(VmError::new("third arg must be string".into()));
            };
            let width_val = *width;
            if width_val < 0 {
                return Err(VmError::new(format!(
                    "string.pad_left: negative index {width_val}"
                )));
            }
            if width_val as u128 > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "string.pad_left: width {width_val} exceeds maximum of {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let width = width_val as usize;
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.chars().count() >= width {
                Ok(Value::String(s.clone()))
            } else {
                let padding: String = (0..width - s.chars().count()).map(|_| pad_char).collect();
                Ok(Value::String(format!("{padding}{s}")))
            }
        }
        "pad_right" => {
            if args.len() != 3 {
                return Err(VmError::new("string.pad_right takes 3 arguments".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("first arg must be string".into()));
            };
            let Value::Int(width) = &args[1] else {
                return Err(VmError::new("second arg must be int".into()));
            };
            let Value::String(pad) = &args[2] else {
                return Err(VmError::new("third arg must be string".into()));
            };
            let width_val = *width;
            if width_val < 0 {
                return Err(VmError::new(format!(
                    "string.pad_right: negative index {width_val}"
                )));
            }
            if width_val as u128 > MAX_RANGE_MATERIALIZE as u128 {
                return Err(VmError::new(format!(
                    "string.pad_right: width {width_val} exceeds maximum of {MAX_RANGE_MATERIALIZE}"
                )));
            }
            let width = width_val as usize;
            let pad_char = pad.chars().next().unwrap_or(' ');
            if s.chars().count() >= width {
                Ok(Value::String(s.clone()))
            } else {
                let padding: String = (0..width - s.chars().count()).map(|_| pad_char).collect();
                Ok(Value::String(format!("{s}{padding}")))
            }
        }
        "char_code" => {
            if args.len() != 1 {
                return Err(VmError::new("string.char_code takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.char_code requires a string".into()));
            };
            match s.chars().next() {
                Some(c) => Ok(Value::Int(c as i64)),
                None => Err(VmError::new("string.char_code: empty string".into())),
            }
        }
        "from_char_code" => {
            if args.len() != 1 {
                return Err(VmError::new(
                    "string.from_char_code takes 1 argument".into(),
                ));
            }
            let Value::Int(n) = &args[0] else {
                return Err(VmError::new("string.from_char_code requires an int".into()));
            };
            // Reject negatives and values outside u32 range before casting,
            // then let char::from_u32 catch surrogates and >0x10FFFF values.
            // Unchecked `as u32` would silently wrap (e.g. 4294967337 -> 41 = ')').
            match u32::try_from(*n).ok().and_then(char::from_u32) {
                Some(c) => Ok(Value::String(c.to_string())),
                None => Err(VmError::new(format!("invalid code point {n}"))),
            }
        }
        "is_empty" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_empty takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_empty requires a string".into()));
            };
            Ok(Value::Bool(s.is_empty()))
        }
        "is_alpha" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_alpha takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_alpha requires a string".into()));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_alphabetic()),
            ))
        }
        "is_digit" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_digit takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_digit requires a string".into()));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()),
            ))
        }
        "is_upper" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_upper takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_upper requires a string".into()));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_uppercase()),
            ))
        }
        "is_lower" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_lower takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_lower requires a string".into()));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_lowercase()),
            ))
        }
        "is_alnum" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_alnum takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new("string.is_alnum requires a string".into()));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_alphanumeric()),
            ))
        }
        "is_whitespace" => {
            if args.len() != 1 {
                return Err(VmError::new("string.is_whitespace takes 1 argument".into()));
            }
            let Value::String(s) = &args[0] else {
                return Err(VmError::new(
                    "string.is_whitespace requires a string".into(),
                ));
            };
            Ok(Value::Bool(
                !s.is_empty() && s.chars().all(|c| c.is_whitespace()),
            ))
        }
        _ => Err(VmError::new(format!("unknown string function: {name}"))),
    }
}
