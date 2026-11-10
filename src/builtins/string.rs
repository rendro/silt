//! String builtin functions (`string.*`).

use super::typed::{List, builtins};
use crate::value::{MAX_COLLECTION_LEN, Value};
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
    if width as u128 > MAX_COLLECTION_LEN as u128 {
        return Err(VmError::new(format!(
            "string.{name}: width {width} exceeds maximum of {MAX_COLLECTION_LEN}"
        )));
    }
    let missing = (width as usize).saturating_sub(s.chars().count());
    Ok(std::iter::repeat_n(pad, missing).collect())
}

/// The byte offset of the character at index `chars` of `s`, the end
/// of `s` included; `None` beyond it. Where the first `chars` bytes are
/// ASCII, each of them is a character and the index is the offset: no
/// walk over the characters. Otherwise the characters are counted
/// eight bytes at a time (a character starts at every byte that is no
/// UTF-8 continuation byte, `0b10xx_xxxx`), and byte by byte in the
/// last word.
fn byte_offset(s: &str, chars: usize) -> Option<usize> {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let bytes = s.as_bytes();
    if bytes[..chars.min(bytes.len())].is_ascii() {
        return (chars <= bytes.len()).then_some(chars);
    }
    // `left` characters start at or after `at`, before the one wanted.
    let (mut at, mut left) = (0, chars);
    while let Some(word) = bytes.get(at..at + 8) {
        let word = u64::from_le_bytes(word.try_into().ok()?);
        // The high bit of each continuation byte: bit 7 set, bit 6 not.
        let continuing = word & !(word << 1) & HIGH;
        let starts = 8 - continuing.count_ones() as usize;
        if starts > left {
            break;
        }
        (at, left) = (at + 8, left - starts);
    }
    // (`at` may be inside a character: its continuation bytes start
    // none.)
    (at..bytes.len())
        .filter(|&i| bytes[i] & 0xC0 != 0x80)
        .chain(std::iter::once(bytes.len()))
        .nth(left)
}

/// The characters `start..end` of `s`, both indexes clamped to its
/// length; empty where `start` is not before `end`. What it costs is
/// the walk to `start` (none over ASCII) and the piece: the rest of
/// `s` is not looked at, so a scan that takes a few characters at a
/// time is not quadratic in the length of `s`.
fn char_range(s: &str, start: usize, end: usize) -> &str {
    let from = byte_offset(s, start).unwrap_or(s.len());
    let rest = &s[from..];
    let to = byte_offset(rest, end.saturating_sub(start)).unwrap_or(rest.len());
    &rest[..to]
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
        vm.shown_within(x, "string.from")
    }

    // An empty separator splits into the characters, with no empty
    // piece at either end.
    fn split(s: &str, separator: &str) -> Vec<Value> {
        if separator.is_empty() {
            return s
                .chars()
                .map(|c| Value::String(c.to_string().into()))
                .collect();
        }
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
        // `MAX_COLLECTION_LEN`; mirror that exactly.
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
        if result_len > MAX_COLLECTION_LEN as u128 {
            return Err(VmError::new(format!(
                "string.replace: result would exceed maximum string size ({} bytes > {} limit)",
                result_len, MAX_COLLECTION_LEN
            )));
        }
        Ok(s.replace(from, to))
    }

    // The size is counted before anything is copied: the items can be
    // one string many times.
    fn join(xs: List, sep: &str) -> Result<String, VmError> {
        xs.writable()?;
        let items = xs.len() as u128;
        let mut result_len = (sep.len() as u128).saturating_mul(items.saturating_sub(1));
        for item in xs.iter() {
            if let Value::String(s) = item {
                result_len = result_len.saturating_add(s.len() as u128);
            }
        }
        if result_len > MAX_COLLECTION_LEN as u128 {
            return Err(VmError::new(format!(
                "string.join: result would exceed maximum string size ({} bytes > {} limit)",
                result_len, MAX_COLLECTION_LEN
            )));
        }
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
        if result_len > MAX_COLLECTION_LEN as u128 {
            return Err(VmError::new(format!(
                "string.repeat: result would exceed maximum string size ({} bytes > {} limit)",
                result_len, MAX_COLLECTION_LEN
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

    // Character indexing (consistent with string.index_of /
    // string.slice): every index up to the length is a boundary, and
    // any other index, a negative one too, is `None`.
    fn split_at(s: &str, idx: i64) -> Option<Value> {
        let boundary = byte_offset(s, usize::try_from(idx).ok()?)?;
        let (left, right) = s.split_at(boundary);
        Some(Value::tuple(vec![
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
        let index = |at: i64| usize::try_from(at).unwrap_or(usize::MAX);
        Ok(char_range(s, index(start), index(end)).to_string())
    }

    fn pad_left(s: &str, width: i64, pad: &str) -> Result<String, VmError> {
        Ok(padding("pad_left", s, width, pad)? + s)
    }

    fn pad_right(s: &str, width: i64, pad: &str) -> Result<String, VmError> {
        Ok(s.to_string() + &padding("pad_right", s, width, pad)?)
    }

    fn char_code(s: &str) -> Option<Value> {
        s.chars().next().map(|c| Value::Int(c as i64))
    }

    // Negatives and values outside the u32 range are no code points
    // (an unchecked `as u32` would wrap: 4294967337 -> 41 = ')'), and
    // `char::from_u32` says so of surrogates and of values above
    // 0x10FFFF.
    fn from_char_code(code: i64) -> Option<Value> {
        let c = u32::try_from(code).ok().and_then(char::from_u32)?;
        Some(Value::String(c.to_string().into()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `string.slice` as it was first written: every character of `s`
    /// in a vector, the indexes clamped to its length.
    fn by_vector(s: &str, start: usize, end: usize) -> String {
        let chars: Vec<char> = s.chars().collect();
        let (start, end) = (start.min(chars.len()), end.min(chars.len()));
        match start > end {
            true => String::new(),
            false => chars[start..end].iter().collect(),
        }
    }

    /// Short texts, and longer ones whose characters are counted eight
    /// bytes at a time, with characters across the words' edges.
    fn texts() -> Vec<String> {
        let short = ["", "abc", "é", "aé", "éa", "日本語abc", "a😀b\u{301}c"];
        let mut texts: Vec<String> = short.iter().map(|s| s.to_string()).collect();
        texts.push("aé日😀".repeat(6));
        texts.push(format!("{}abc", "é".repeat(13)));
        texts.push(format!("{}é{}", "a".repeat(9), "😀b".repeat(7)));
        texts.push("日".repeat(11));
        texts
    }

    #[test]
    fn a_byte_offset_is_that_of_the_character_at_the_index() {
        for s in &texts() {
            let mut offsets: Vec<usize> = s.char_indices().map(|(at, _)| at).collect();
            offsets.push(s.len());
            for chars in 0..offsets.len() + 3 {
                assert_eq!(
                    byte_offset(s, chars),
                    offsets.get(chars).copied(),
                    "{s:?} at {chars}"
                );
            }
        }
    }

    #[test]
    fn a_range_of_characters_is_what_the_vector_of_them_gives() {
        for s in &texts() {
            let most = s.chars().count() + 3;
            for start in 0..most {
                for end in 0..most {
                    assert_eq!(
                        char_range(s, start, end),
                        by_vector(s, start, end),
                        "{s:?} {start}..{end}"
                    );
                }
            }
            assert_eq!(char_range(s, 0, usize::MAX), s.as_str());
            assert_eq!(char_range(s, usize::MAX, usize::MAX), "");
        }
    }

    /// A piece near the start of a long string costs what the piece
    /// costs, not the length of the string. A million slices of a
    /// string of four million characters: before, each one copied the
    /// whole string into a vector of characters (about 4 * 10^12
    /// characters in all, hours); now each looks at a few dozen bytes.
    #[test]
    fn a_piece_near_the_start_does_not_cost_the_length_of_the_string() {
        let long = 4_000_000;
        for s in [
            "a".repeat(long),
            "é".repeat(long),
            format!("é{}", "a".repeat(long)),
        ] {
            let first = s.chars().next().map(String::from).unwrap_or_default();
            let tenth: String = s.chars().skip(10).take(10).collect();
            for _ in 0..1_000_000 {
                assert_eq!(char_range(&s, 0, 1), first);
            }
            for _ in 0..100_000 {
                assert_eq!(char_range(&s, 10, 20), tenth);
            }
        }
    }
}
