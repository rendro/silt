//! `csv.*`: comma-separated values as RFC 4180 writes them. A field is
//! quoted when it holds a comma, a quote or a line break: `"Smith, John"`,
//! a quote in it doubled (`"say ""hi"""`). Records end at a line break
//! outside quotes, `\n` or `\r\n`.
//!
//! The reader is lenient where RFC 4180 is strict and every reader in
//! use is too: a quote inside a field that does not start with one
//! (`5" disk`) is part of the field. It refuses what has no one reading:
//! a quoted field that is not closed, and text after a closing quote
//! before the next comma or line end.

use super::common::ok;
use super::typed::builtins;
use crate::typeinfo::bv;
use crate::value::Value;

/// What `CsvError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Syntax", [Value::String(m), Value::Int(offset)]) => {
            format!("csv syntax error at byte {offset}: {m}")
        }
        ("TypeMismatch", [Value::String(exp), Value::String(act)]) => {
            format!("csv type mismatch: expected {exp}, got {act}")
        }
        ("MissingField", [Value::String(n)]) => format!("csv missing field: {n}"),
        ("Other", [Value::String(m)]) => m.to_string(),
        _ => return None,
    })
}

/// Why a text is no CSV, and the byte of the text where.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Malformed {
    pub(crate) why: &'static str,
    pub(crate) at: usize,
}

/// A quoted field that the text ends in.
const UNCLOSED: &str = "a quoted field is not closed";
/// Text after the quote that closes a field.
const AFTER_QUOTE: &str = "expected a comma or the end of the line after a quoted field";

/// The record that starts at byte `start` of `text`, and the byte after
/// the line break that ends it (the text's length at its end). A record
/// ends at a line break outside quotes, `\n` or `\r\n`, or at the end of
/// the text.
pub(crate) fn record(text: &str, start: usize) -> Result<(Vec<String>, usize), Malformed> {
    let bytes = text.as_bytes();
    let mut fields = Vec::new();
    let mut at = start;
    loop {
        if bytes.get(at) == Some(&b'"') {
            let opened = at;
            let mut field = String::new();
            let mut from = at + 1;
            loop {
                let Some(quote) = bytes[from..].iter().position(|b| *b == b'"') else {
                    return Err(Malformed {
                        why: UNCLOSED,
                        at: opened,
                    });
                };
                let quote = from + quote;
                field.push_str(&text[from..quote]);
                if bytes.get(quote + 1) == Some(&b'"') {
                    field.push('"');
                    from = quote + 2;
                    continue;
                }
                at = quote + 1;
                break;
            }
            fields.push(field);
            match bytes.get(at) {
                Some(b',') => at += 1,
                None => return Ok((fields, at)),
                Some(b'\n') => return Ok((fields, at + 1)),
                Some(b'\r') if bytes.get(at + 1) == Some(&b'\n') => return Ok((fields, at + 2)),
                Some(_) => {
                    return Err(Malformed {
                        why: AFTER_QUOTE,
                        at,
                    });
                }
            }
        } else {
            let end = bytes[at..]
                .iter()
                .position(|b| *b == b',' || *b == b'\n')
                .map_or(bytes.len(), |n| at + n);
            match bytes.get(end) {
                Some(b',') => {
                    fields.push(text[at..end].to_string());
                    at = end + 1;
                }
                // A line break: `\r\n` ends the record as `\n` does.
                Some(_) => {
                    let field_end = match end > at && bytes[end - 1] == b'\r' {
                        true => end - 1,
                        false => end,
                    };
                    fields.push(text[at..field_end].to_string());
                    return Ok((fields, end + 1));
                }
                None => {
                    fields.push(text[at..end].to_string());
                    return Ok((fields, end));
                }
            }
        }
    }
}

/// The fields of one line: one line break at its end (`\n` or `\r\n`,
/// as a line read from a file may keep) is not part of it; another
/// outside quotes is an error.
pub(crate) fn fields_of_line(line: &str) -> Result<Vec<String>, Malformed> {
    let (fields, next) = record(line, 0)?;
    match next < line.len() {
        true => Err(Malformed {
            why: "a line break outside quotes: csv.split_line reads one line",
            at: line[..next].rfind('\n').unwrap_or(next),
        }),
        false => Ok(fields),
    }
}

/// `Err(Syntax(why, at))`.
pub(crate) fn syntax_err(malformed: &Malformed) -> Value {
    let at = i64::try_from(malformed.at).unwrap_or(i64::MAX);
    Value::variant(
        bv::ERR,
        [Value::variant(
            bv::CSV_SYNTAX,
            [Value::String(malformed.why.into()), Value::Int(at)],
        )],
    )
}

builtins! {
    fn split_line(line: &str) -> Value {
        match fields_of_line(line) {
            Ok(fields) => ok(Value::list(
                fields.into_iter().map(|f| Value::String(f.into())).collect(),
            )),
            Err(malformed) => syntax_err(&malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AFTER_QUOTE, Malformed, UNCLOSED, fields_of_line as split_line, record};

    fn fields(line: &str) -> Vec<String> {
        split_line(line).unwrap_or_else(|e| panic!("{line:?}: {e:?}"))
    }

    #[test]
    fn a_line_is_split_at_its_commas_outside_quotes() {
        assert_eq!(fields("a,b,c"), ["a", "b", "c"]);
        assert_eq!(fields(""), [""]);
        assert_eq!(fields(","), ["", ""]);
        assert_eq!(fields("a,"), ["a", ""]);
        assert_eq!(fields(" a , b "), [" a ", " b "]);
        assert_eq!(fields("\"Smith, John\",42"), ["Smith, John", "42"]);
        assert_eq!(fields("\"say \"\"hi\"\"\",x"), ["say \"hi\"", "x"]);
        assert_eq!(fields("\"\""), [""]);
        assert_eq!(fields("\"\",\"\""), ["", ""]);
        assert_eq!(fields("\"two\nlines\",b"), ["two\nlines", "b"]);
        assert_eq!(fields("\"crlf\r\ninside\""), ["crlf\r\ninside"]);
        // A quote inside a field that starts with none is the field's.
        assert_eq!(fields("5\" disk,a\"b"), ["5\" disk", "a\"b"]);
        assert_eq!(fields("é,ü"), ["é", "ü"]);
        // One line break at the end is not the line's.
        assert_eq!(fields("a,b\n"), ["a", "b"]);
        assert_eq!(fields("a,b\r\n"), ["a", "b"]);
        assert_eq!(fields("\"a\"\r\n"), ["a"]);
        // A lone carriage return is text.
        assert_eq!(fields("a\rb"), ["a\rb"]);
    }

    #[test]
    fn a_line_that_is_no_csv_is_refused_where_it_goes_wrong() {
        let refused = |line: &str| split_line(line).expect_err(line);
        assert_eq!(
            refused("a,\"bc"),
            Malformed {
                why: UNCLOSED,
                at: 2
            }
        );
        assert_eq!(
            refused("\"a\"b,c"),
            Malformed {
                why: AFTER_QUOTE,
                at: 3
            }
        );
        assert_eq!(
            refused("\"a\" ,c"),
            Malformed {
                why: AFTER_QUOTE,
                at: 3
            }
        );
        assert_eq!(refused("a,b\nc").at, 3);
        assert_eq!(refused("a\n\n").at, 1);
    }

    #[test]
    fn records_follow_one_another_in_a_text() {
        let text = "h1,h2\r\n\"x\ny\",2\nlast,\"\"";
        let (first, next) = record(text, 0).expect("a record");
        assert_eq!((first, next), (vec!["h1".into(), "h2".into()], 7));
        let (second, next) = record(text, next).expect("a record");
        assert_eq!(second, ["x\ny", "2"]);
        let (third, next) = record(text, next).expect("a record");
        assert_eq!(
            (third, next),
            (vec!["last".into(), String::new()], text.len())
        );
    }
}
