//! String builtin functions (`string.*`).

use super::common::cut;
use super::typed::{List, Text, builtins};
use crate::value::{MAX_COLLECTION_LEN, Value};
use crate::vm::{Step, Vm, VmError};

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
/// if it is that wide already (a negative width is 0).
fn padding(name: &str, s: &str, width: i64, pad: &str) -> Result<String, VmError> {
    let pad = pad_char(name, pad)?;
    fits_width(name, width)?;
    Ok(fill(s, width, pad))
}

/// `width` is no wider than a string may be, for `string.<name>`.
fn fits_width(name: &str, width: i64) -> Result<(), VmError> {
    match width as i128 > MAX_COLLECTION_LEN as i128 {
        true => Err(VmError::new(format!(
            "string.{name}: width {width} exceeds maximum of {MAX_COLLECTION_LEN}"
        ))),
        false => Ok(()),
    }
}

/// The `pad`s that fill `s` up to `width` characters.
fn fill(s: &str, width: i64, pad: char) -> String {
    let missing = cut(width).saturating_sub(s.chars().count());
    std::iter::repeat_n(pad, missing).collect()
}

/// `string.left` and `string.right`: `x` as `Display` shows it, with
/// spaces after it (`at_left`) or before it up to `width` characters.
fn aligned(vm: &mut Vm, x: &Value, width: i64, at_left: bool) -> Result<Step, VmError> {
    let (name, builtin) = match at_left {
        true => ("left", "string.left"),
        false => ("right", "string.right"),
    };
    fits_width(name, width)?;
    vm.show_within(x, Some(builtin), move |_, text| {
        let spaces = fill(&text, width, ' ');
        let line = match at_left {
            true => text + &spaces,
            false => spaces + &text,
        };
        Ok(Step::Done(Value::string(line)))
    })
}

/// The byte offset of the character at index `chars` of `s`, the end
/// of `s` included; `None` beyond it. In a string marked all ASCII
/// (`ascii`) each byte is a character and the index is the offset, at
/// once. So it is where the first `chars` bytes are ASCII, which is
/// looked at first: no walk over the characters. Otherwise the
/// characters are counted eight bytes at a time (a character starts at
/// every byte that is no UTF-8 continuation byte, `0b10xx_xxxx`), and
/// byte by byte in the last word.
fn byte_offset(s: &str, ascii: bool, chars: usize) -> Option<usize> {
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let bytes = s.as_bytes();
    if ascii || bytes[..chars.min(bytes.len())].is_ascii() {
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

/// The characters `start..end` of `s` (marked all ASCII if `ascii`),
/// both indexes clamped to its length; empty where `start` is not
/// before `end`. What it costs is the walk to `start` (none over ASCII)
/// and the piece: the rest of `s` is not looked at, so a scan that
/// takes a few characters at a time is not quadratic in the length of
/// `s`.
fn char_range(s: &str, ascii: bool, start: usize, end: usize) -> &str {
    let from = byte_offset(s, ascii, start).unwrap_or(s.len());
    let rest = &s[from..];
    let to = byte_offset(rest, ascii, end.saturating_sub(start)).unwrap_or(rest.len());
    &rest[..to]
}

/// The character index of the byte offset `at` of `s` (its byte offset
/// in a string marked all ASCII), as `Some`; or `None`.
fn char_index(s: Text, at: Option<usize>) -> Option<Value> {
    let index = |at: usize| match s.ascii {
        true => at,
        false => s.text[..at].chars().count(),
    };
    at.map(|at| Value::Int(index(at) as i64))
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
                .map(|c| Value::string(c.to_string()))
                .collect();
        }
        s.split(separator)
            .map(Value::string)
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
            if let Value::String(s, _) = item {
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

    // Constant time for a string marked all ASCII.
    fn length(s: Text) -> i64 {
        match s.ascii {
            true => s.text.len() as i64,
            false => s.text.chars().count() as i64,
        }
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
        s.chars().map(|c| Value::string(c.encode_utf8(&mut [0; 4]))).collect()
    }

    // A negative count is 0.
    fn repeat(s: &str, n: i64) -> Result<String, VmError> {
        let n = cut(n);
        let result_len = (n as u128) * (s.len() as u128);
        if result_len > MAX_COLLECTION_LEN as u128 {
            return Err(VmError::new(format!(
                "string.repeat: result would exceed maximum string size ({} bytes > {} limit)",
                result_len, MAX_COLLECTION_LEN
            )));
        }
        Ok(s.repeat(n))
    }

    // (A character index, as `string.slice` takes them: not a byte's.)
    fn index_of(s: Text, needle: &str) -> Option<Value> {
        char_index(s, s.text.find(needle))
    }

    fn last_index_of(s: Text, needle: &str) -> Option<Value> {
        char_index(s, s.text.rfind(needle))
    }

    // Character indexing (consistent with string.index_of /
    // string.slice): every index up to the length is a boundary, and
    // any other index, a negative one too, is `None`.
    fn split_at(s: Text, idx: i64) -> Option<Value> {
        let boundary = byte_offset(s.text, s.ascii, usize::try_from(idx).ok()?)?;
        let (left, right) = s.text.split_at(boundary);
        Some(Value::tuple(vec![
            Value::string(left),
            Value::string(right),
        ]))
    }

    // At the first `separator`: the text before it and the text after
    // it, or `None` if it is not in `s`. (An empty separator is found
    // at the start, as `index_of` finds it.)
    fn split_once(s: &str, separator: &str) -> Option<Value> {
        let (left, right) = s.split_once(separator)?;
        Some(Value::tuple(vec![
            Value::string(left),
            Value::string(right),
        ]))
    }

    fn strip_prefix(s: &str, prefix: &str) -> Option<Value> {
        s.strip_prefix(prefix).map(Value::string)
    }

    fn strip_suffix(s: &str, suffix: &str) -> Option<Value> {
        s.strip_suffix(suffix).map(Value::string)
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
            lines.push(Value::string(trimmed));
        }
        lines
    }

    // A predicate: an offset that is not in the string is `false`, not
    // an error. Character indexing, consistent with string.index_of /
    // string.slice.
    fn starts_with_at(s: Text, offset: i64, prefix: &str) -> bool {
        usize::try_from(offset)
            .ok()
            .and_then(|offset| byte_offset(s.text, s.ascii, offset))
            .is_some_and(|at| s.text[at..].starts_with(prefix))
    }

    // Both indexes are cut to the string: a negative one is 0, one
    // past the end is the end.
    fn slice(s: Text, start: i64, end: i64) -> String {
        char_range(s.text, s.ascii, cut(start), cut(end)).to_string()
    }

    // `x` as `Display` shows it, at the left of `width` characters:
    // spaces after it.
    fn left(vm, x: &Value, width: i64) -> Result<Step, VmError> {
        aligned(vm, x, width, true)
    }

    // `x` as `Display` shows it, at the right of `width` characters:
    // spaces before it.
    fn right(vm, x: &Value, width: i64) -> Result<Step, VmError> {
        aligned(vm, x, width, false)
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
        Some(Value::string(c.to_string()))
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
                    byte_offset(s, s.is_ascii(), chars),
                    offsets.get(chars).copied(),
                    "{s:?} at {chars}"
                );
                assert_eq!(
                    byte_offset(s, false, chars),
                    offsets.get(chars).copied(),
                    "{s:?} at {chars}, not marked"
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
                        char_range(s, s.is_ascii(), start, end),
                        by_vector(s, start, end),
                        "{s:?} {start}..{end}"
                    );
                }
            }
            assert_eq!(char_range(s, s.is_ascii(), 0, usize::MAX), s.as_str());
            assert_eq!(char_range(s, s.is_ascii(), usize::MAX, usize::MAX), "");
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
            let ascii = s.is_ascii();
            for _ in 0..1_000_000 {
                assert_eq!(char_range(&s, ascii, 0, 1), first);
            }
            for _ in 0..100_000 {
                assert_eq!(char_range(&s, ascii, 10, 20), tenth);
            }
        }
    }

    /// In a string marked all ASCII an offset costs nothing, wherever it
    /// is: a scan by index of every character of a string of four
    /// million, each taken at its offset and its index found. Were
    /// each offset counted from the start again (if only by looking at
    /// the bytes before it, eight or more at a time), that would be
    /// some 10^12 bytes looked at: hours, not the moment it takes.
    #[test]
    fn an_offset_into_a_string_marked_ascii_costs_nothing() {
        let long = 4_000_000;
        let s = "ab".repeat(long / 2);
        let value = Value::string(s.as_str());
        let Value::String(text, ascii) = &value else {
            unreachable!("a string");
        };
        assert!(ascii.is());
        let text = Text {
            text,
            ascii: ascii.is(),
        };
        let mut a = 0;
        for i in 0..long {
            let piece = char_range(text.text, text.ascii, i, i + 1);
            a += usize::from(piece == "a");
            assert_eq!(byte_offset(text.text, text.ascii, i), Some(i));
            assert!(char_index(text, Some(i)).is_some());
        }
        assert_eq!(a, long / 2);
    }

    #[test]
    fn a_string_is_marked_ascii_when_it_is() {
        for (s, ascii) in [
            ("", true),
            ("abc ~\u{7f}", true),
            ("\u{7f}", true),
            ("é", false),
            ("abc\u{80}", false),
            ("日本", false),
        ] {
            let Value::String(_, mark) = Value::string(s) else {
                unreachable!("a string");
            };
            assert_eq!(mark.is(), ascii, "{s:?}");
        }
    }
}
