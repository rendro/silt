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

use serde::de::value::MapDeserializer;
use serde::de::{self, DeserializeSeed, SeqAccess, Visitor};

use super::common::ok;
use super::decode::{self, Decoder, Numbers, Rules};
use super::registry::decoding::Family;
use super::typed::{Type, builtins};
use crate::typeinfo::{FieldType, bv};
use crate::value::Value;
use crate::vm::{Source, Step, Vm, VmError};

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

// ── A whole text ────────────────────────────────────────────────────

/// How the one decoder reads the rows of a CSV text (`csv.parse`).
pub(crate) const RULES: Rules = Rules {
    caller: "csv.parse",
    table: "row",
    variant: "String",
    numbers: Numbers::Values,
    family: Family::Csv,
    errors: [bv::CSV_TYPE_MISMATCH, bv::CSV_MISSING_FIELD, bv::CSV_OTHER],
    // (A CSV cell is given to no impl: the checker refuses a type with
    // one, and the decoder honours none for csv.)
    from_impl: |error, _| error.clone(),
};

/// A CSV text read into its header and rows, each row with the line it
/// starts on (the header's is 1).
pub(crate) struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    lines: Vec<usize>,
}

/// `Err(Syntax("line {line}: {why}", at))`.
fn syntax_at(line: usize, why: impl std::fmt::Display, at: usize) -> Value {
    let at = i64::try_from(at).unwrap_or(i64::MAX);
    Value::variant(
        bv::ERR,
        [Value::variant(
            bv::CSV_SYNTAX,
            [
                Value::String(format!("line {line}: {why}").into()),
                Value::Int(at),
            ],
        )],
    )
}

/// The header and rows of `text`: records end at a line break outside
/// quotes; a UTF-8 byte order mark before the header is dropped; the
/// empty line after a final line break is no row. A row with another
/// number of fields than the header, or a header that names a column
/// twice, is a `Syntax` error, which names its line.
pub(crate) fn table(text: &str) -> Result<Table, Value> {
    let start = match text.starts_with('\u{feff}') {
        true => '\u{feff}'.len_utf8(),
        false => 0,
    };
    let mut records = Vec::new();
    let (mut at, mut line) = (start, 1);
    while at < text.len() {
        let (fields, next) = record(text, at).map_err(|m| {
            let line = line + text[at..m.at].matches('\n').count();
            syntax_at(line, m.why, m.at)
        })?;
        records.push((fields, line, at));
        line += text[at..next].matches('\n').count();
        at = next;
    }
    let mut records = records.into_iter();
    let Some((header, _, _)) = records.next() else {
        return Ok(Table {
            header: Vec::new(),
            rows: Vec::new(),
            lines: Vec::new(),
        });
    };
    for (k, name) in header.iter().enumerate() {
        if header[..k].contains(name) {
            let why = format!("the header names the column `{name}` twice");
            return Err(syntax_at(1, why, start));
        }
    }
    let (mut rows, mut lines) = (Vec::new(), Vec::new());
    for (fields, line, at) in records {
        if fields.len() != header.len() {
            let why = format!(
                "a row of {} fields, and the header has {}",
                fields.len(),
                header.len()
            );
            return Err(syntax_at(line, why, at));
        }
        rows.push(fields);
        lines.push(line);
    }
    Ok(Table {
        header,
        rows,
        lines,
    })
}

/// The rows of a [`Table`] as the decoder reads a document: an array of
/// rows, each a map of its cells by their column's name.
struct Rows<'t>(&'t Table);

impl<'de> de::Deserializer<'de> for Rows<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(RowSeq {
            table: self.0,
            next: 0,
        })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

struct RowSeq<'t> {
    table: &'t Table,
    next: usize,
}

impl<'de> SeqAccess<'de> for RowSeq<'_> {
    type Error = de::value::Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, Self::Error> {
        let Some(row) = self.table.rows.get(self.next) else {
            return Ok(None);
        };
        self.next += 1;
        let header = &self.table.header;
        seed.deserialize(Row(header, row)).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.table.rows.len() - self.next)
    }
}

struct Row<'t>(&'t [String], &'t [String]);

impl<'de> de::Deserializer<'de> for Row<'_> {
    type Error = de::value::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let cells = (self.0.iter().map(String::as_str)).zip(self.1.iter().map(String::as_str));
        visitor.visit_map(MapDeserializer::new(cells))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum identifier ignored_any
    }
}

/// The text of the decoder's error `message`, which ends with the path of
/// the value (` at [2].age`), with the line and the column instead
/// (` at line 4, column age`).
fn at_line(message: &str, lines: &[usize]) -> String {
    let Some((before, path)) = message.rsplit_once(" at [") else {
        return message.to_string();
    };
    let Some((index, rest)) = path.split_once(']') else {
        return message.to_string();
    };
    let Some(line) = index.parse::<usize>().ok().and_then(|i| lines.get(i)) else {
        return message.to_string();
    };
    match rest.strip_prefix('.') {
        Some(column) => format!("{before} at line {line}, column {column}"),
        None => format!("{before} at line {line}"),
    }
}

/// The result `result` of the decoder with each error's path a line and
/// a column ([`at_line`]).
fn with_lines(result: Value, lines: &[usize]) -> Value {
    let Value::Variant(outer) = &result else {
        return result;
    };
    let [Value::Variant(error)] = outer.fields() else {
        return result;
    };
    if !outer.is(bv::ERR) {
        return result;
    }
    let text = |v: &Value| match v {
        Value::String(s) => Value::String(at_line(s, lines).into()),
        other => other.clone(),
    };
    let fields: Vec<Value> = match error.fields() {
        [expected, actual] if error.is(bv::CSV_TYPE_MISMATCH) => {
            vec![expected.clone(), text(actual)]
        }
        [message] if error.is(bv::CSV_OTHER) => vec![text(message)],
        _ => return result,
    };
    Value::variant(bv::ERR, [Value::variant(error.tag(), fields)])
}

/// A CSV text read again, for the decoder.
struct Text(std::sync::Arc<Table>);

impl Source for Text {
    fn read(
        &self,
        r: &decode::Reader,
        expected: &FieldType,
        parts: &decode::seed::Parts,
    ) -> Result<decode::seed::Outcome, Value> {
        decode::seed::csv_rows(r, expected, Rows(&self.0), parts).map_err(other_err)
    }
}

/// `Err(Other(message))`.
fn other_err(message: String) -> Value {
    Value::variant(
        bv::ERR,
        [Value::variant(
            bv::CSV_OTHER,
            [Value::String(message.into())],
        )],
    )
}

static PARSE: Decoder = Decoder::new("csv.parse");

/// The column the header of `table` lacks for a field of the record type
/// `element` that is no `Option`: a field with no column is missing from
/// every row, also where there is none.
fn missing_column(vm: &Vm, table: &Table, element: &FieldType) -> Option<String> {
    let (id, args) = match element {
        FieldType::Record(id) => (*id, &[][..]),
        FieldType::Applied(id, args) => (*id, args.as_slice()),
        _ => return None,
    };
    let info = vm.runtime.type_info(id)?;
    info.fields()
        .iter()
        .find(|(name, field)| {
            !matches!(&*field.instantiated(args), FieldType::Option(_))
                && !table.header.contains(name)
        })
        .map(|(name, _)| name.clone())
}

builtins! {
    // Every row of the text as a record of the type, its fields read
    // from the columns of their names by the one decoder (as json's and
    // toml's are read).
    fn parse(vm, text: &str, ty: Type) -> Result<Step, VmError> {
        let element = decode::target(&PARSE, ty)?.element();
        let table = match table(text) {
            Ok(table) => std::sync::Arc::new(table),
            Err(syntax) => return Ok(Step::Done(syntax)),
        };
        if table.header.is_empty() {
            return Ok(Step::Done(ok(Value::list(Vec::new()))));
        }
        if let Some(name) = missing_column(vm, &table, &element) {
            return Ok(Step::Done(Value::variant(
                bv::ERR,
                [Value::variant(bv::CSV_MISSING_FIELD, [Value::String(name.into())])],
            )));
        }
        let expected = FieldType::List(Box::new(element));
        let read = {
            let table = table.clone();
            move |r: &decode::Reader, expected: &FieldType, parts: &decode::seed::Parts| {
                decode::seed::csv_rows(r, expected, Rows(&table), parts).map_err(other_err)
            }
        };
        let source = {
            let table = table.clone();
            move || -> Box<dyn Source> { Box::new(Text(table)) }
        };
        let step = vm.decode_read("csv.parse", &RULES, expected, read, source)?;
        Ok(match step {
            Step::Done(result) => Step::Done(with_lines(result, &table.lines)),
            other => other,
        })
    }

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
