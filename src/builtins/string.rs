//! String builtin functions (`string.*`).

use super::typed::{List, builtins};
use crate::value::{MAX_RANGE_MATERIALIZE, Value};
use crate::vm::{Step, VmError};

/// The one character `pad` is, for `string.<name>`.
fn pad_char(name: &str, pad: &str) -> Result<char, VmError> {
    let mut chars = pad.chars();
    let Some(first) = chars.next() else {
        return Err(VmError::new(format!(
            "string.{name}: pad must be a non-empty 1-character string, got \"\""
        )));
    };
    if chars.next().is_some() {
        let count = pad.chars().count();
        return Err(VmError::new(format!(
            "string.{name}: pad must be a 1-character string, got {pad:?} ({count} characters)"
        )));
    }
    Ok(first)
}

/// The characters that pad `s` to `width`, for `string.<name>`: none
/// if it is that wide already.
fn padding(name: &str, s: &str, width: i64, pad: &str) -> Result<String, VmError> {
    let pad = pad_char(name, pad)?;
    if width < 0 {
        return Err(VmError::new(format!(
            "string.{name}: negative width {width}"
        )));
    }
    if width as u128 > MAX_RANGE_MATERIALIZE as u128 {
        return Err(VmError::new(format!(
            "string.{name}: width {width} exceeds maximum of {MAX_RANGE_MATERIALIZE}"
        )));
    }
    let missing = (width as usize).saturating_sub(s.chars().count());
    Ok(std::iter::repeat_n(pad, missing).collect())
}

/// The byte offset of the character at index `chars` of `s`, the end
/// of `s` included; `None` beyond it.
fn byte_offset(s: &str, chars: usize) -> Option<usize> {
    s.char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(s.len()))
        .nth(chars)
}

/// The character index of the byte offset `at` of `s`, as `Some`; or
/// `None`.
fn char_index(s: &str, at: Option<usize>) -> Option<Value> {
    at.map(|at| Value::Int(s[..at].chars().count() as i64))
}

/// Whether `s` has a character and all of them are `of`.
fn all(s: &str, of: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.chars().all(of)
}

builtins! {
    // (A value is shown as its `Display` impl says, which may be the
    // program's code: a step, not a value at once.)
    fn from(vm, x: &Value) -> Result<Step, VmError> {
        vm.shown(x)
    }

    fn split(s: &str, separator: &str) -> Vec<Value> {
        s.split(separator)
            .map(|part| Value::String(part.into()))
            .collect()
    }

    fn trim(s: &str) -> String {
        s.trim().to_string()
    }

    fn trim_start(s: &str) -> String {
        s.trim_start().to_string()
    }

    fn trim_end(s: &str) -> String {
        s.trim_end().to_string()
    }

    fn contains(s: &str, sub: &str) -> bool {
        s.contains(sub)
    }

    fn replace(s: &str, from: &str, to: &str) -> Result<String, VmError> {
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
            let occurrences = s.matches(from).count() as u128;
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
        Ok(s.replace(from, to))
    }

    fn join(xs: List, sep: &str) -> Result<String, VmError> {
        xs.writable()?;
        let shown: Vec<String> = xs.iter().map(|item| item.to_string()).collect();
        Ok(shown.join(sep))
    }

    fn length(s: &str) -> i64 {
        s.chars().count() as i64
    }

    fn byte_length(s: &str) -> i64 {
        s.len() as i64
    }

    fn to_upper(s: &str) -> String {
        s.to_uppercase()
    }

    fn to_lower(s: &str) -> String {
        s.to_lowercase()
    }

    fn starts_with(s: &str, prefix: &str) -> bool {
        s.starts_with(prefix)
    }

    fn ends_with(s: &str, suffix: &str) -> bool {
        s.ends_with(suffix)
    }

    fn chars(s: &str) -> Vec<Value> {
        s.chars().map(|c| Value::String(c.encode_utf8(&mut [0; 4]).into())).collect()
    }

    fn repeat(s: &str, n: i64) -> Result<String, VmError> {
        if n < 0 {
            return Err(VmError::new(format!("string.repeat: negative count {n}")));
        }
        let result_len = (n as u128) * (s.len() as u128);
        if result_len > MAX_RANGE_MATERIALIZE as u128 {
            return Err(VmError::new(format!(
                "string.repeat: result would exceed maximum string size ({} bytes > {} limit)",
                result_len, MAX_RANGE_MATERIALIZE
            )));
        }
        Ok(s.repeat(n as usize))
    }

    // (A character index, as `string.slice` takes them: not a byte's.)
    fn index_of(s: &str, needle: &str) -> Option<Value> {
        char_index(s, s.find(needle))
    }

    fn last_index_of(s: &str, needle: &str) -> Option<Value> {
        char_index(s, s.rfind(needle))
    }

    fn split_at(s: &str, idx: i64) -> Result<Value, VmError> {
        if idx < 0 {
            return Err(VmError::new(format!(
                "string.split_at: negative index {idx}"
            )));
        }
        // Character indexing (consistent with string.index_of /
        // string.slice): every index up to the length is a boundary.
        let Some(boundary) = usize::try_from(idx).ok().and_then(|idx| byte_offset(s, idx)) else {
            return Err(VmError::new(format!(
                "string.split_at: index {idx} out of bounds (length {})",
                s.chars().count()
            )));
        };
        let (left, right) = s.split_at(boundary);
        Ok(Value::tuple(vec![
            Value::String(left.into()),
            Value::String(right.into()),
        ]))
    }

    fn lines(s: &str) -> Vec<Value> {
        // Split on '\n' only. A trailing '\n' must NOT produce an empty
        // final element (matches user expectation: "a\nb\n".lines() ==
        // ["a", "b"]). Also strip a trailing '\r' from each line to
        // normalise \r\n line endings. Empty input yields [] (matches
        // Rust's str::lines and Python's str.splitlines).
        if s.is_empty() {
            return Vec::new();
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
            lines.push(Value::String(trimmed.into()));
        }
        lines
    }

    // A predicate: an offset that is not in the string is `false`, not
    // an error. Character indexing, consistent with string.index_of /
    // string.slice.
    fn starts_with_at(s: &str, offset: i64, prefix: &str) -> bool {
        usize::try_from(offset)
            .ok()
            .and_then(|offset| byte_offset(s, offset))
            .is_some_and(|at| s[at..].starts_with(prefix))
    }

    fn slice(s: &str, start: i64, end: i64) -> Result<String, VmError> {
        if let Some(negative) = [start, end].into_iter().find(|index| *index < 0) {
            return Err(VmError::new(format!(
                "string.slice: negative index {negative}"
            )));
        }
        let chars: Vec<char> = s.chars().collect();
        let start = (start as usize).min(chars.len());
        let end = (end as usize).min(chars.len());
        Ok(match start > end {
            true => String::new(),
            false => chars[start..end].iter().collect(),
        })
    }

    fn pad_left(s: &str, width: i64, pad: &str) -> Result<String, VmError> {
        Ok(padding("pad_left", s, width, pad)? + s)
    }

    fn pad_right(s: &str, width: i64, pad: &str) -> Result<String, VmError> {
        Ok(s.to_string() + &padding("pad_right", s, width, pad)?)
    }

    fn char_code(s: &str) -> Result<i64, VmError> {
        match s.chars().next() {
            Some(c) => Ok(c as i64),
            None => Err(VmError::new("string.char_code: empty string".into())),
        }
    }

    fn from_char_code(code: i64) -> Result<String, VmError> {
        // Reject negatives and values outside u32 range before casting,
        // then let char::from_u32 catch surrogates and >0x10FFFF values.
        // Unchecked `as u32` would silently wrap (e.g. 4294967337 -> 41 = ')').
        match u32::try_from(code).ok().and_then(char::from_u32) {
            Some(c) => Ok(c.to_string()),
            None => Err(VmError::new(format!("invalid code point {code}"))),
        }
    }

    fn is_empty(s: &str) -> bool {
        s.is_empty()
    }

    fn is_alpha(s: &str) -> bool {
        all(s, char::is_alphabetic)
    }

    fn is_digit(s: &str) -> bool {
        all(s, |c| c.is_ascii_digit())
    }

    fn is_upper(s: &str) -> bool {
        all(s, char::is_uppercase)
    }

    fn is_lower(s: &str) -> bool {
        all(s, char::is_lowercase)
    }

    fn is_alnum(s: &str) -> bool {
        all(s, char::is_alphanumeric)
    }

    fn is_whitespace(s: &str) -> bool {
        all(s, char::is_whitespace)
    }
}
