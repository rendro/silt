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

use super::common::{ok, too_long_text};
use super::data::{self, Data, Format};
use super::decode::{self, Decoder, Numbers, Rules};
use super::registry::decoding::Family;
use super::typed::{self, Type, builtins, unsound};
use crate::typeinfo::{FieldType, bv};
use crate::value::{MAX_COLLECTION_LEN, Value};
use crate::vm::{Source, Step, Vm, VmError};

/// What `CsvError`'s `message` says of the variant `tag` with `fields`:
/// `None` if they are no variant of it.
pub(crate) fn error_text(tag: &str, fields: &[Value]) -> Option<String> {
    Some(match (tag, fields) {
        ("Syntax", [Value::String(m, _), Value::Int(offset)]) => {
            format!("csv syntax error at byte {offset}: {m}")
        }
        ("TypeMismatch", [Value::String(exp, _), Value::String(act, _)]) => {
            format!("csv type mismatch: expected {exp}, got {act}")
        }
        ("MissingField", [Value::String(n, _)]) => format!("csv missing field: {n}"),
        ("Other", [Value::String(m, _)]) => m.to_string(),
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
/// A carriage return that is not part of a line end.
const BARE_CR: &str = "a carriage return (\\r) outside quotes: csv lines end in \\n or \\r\\n";

/// The record that starts at byte `start` of `text`, and the byte after
/// the line break that ends it (the text's length at its end). A record
/// ends at a line break outside quotes, `\n` or `\r\n`, or at the end of
/// the text.
pub(crate) fn record(text: &str, start: usize) -> Result<(Vec<String>, usize), Malformed> {
    record_at(text, start).map(|(fields, _, next)| (fields, next))
}

/// [`record`], with the byte at which each field starts.
fn record_at(text: &str, start: usize) -> Result<(Vec<String>, Vec<usize>, usize), Malformed> {
    let bytes = text.as_bytes();
    let mut fields = Vec::new();
    let mut starts = Vec::new();
    let mut at = start;
    // A carriage return outside quotes is part of a line end, `\r\n`,
    // or no CSV: RFC 4180's text has none.
    let bare_cr = |at| Malformed { why: BARE_CR, at };
    loop {
        starts.push(at);
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
                None => return Ok((fields, starts, at)),
                Some(b'\n') => return Ok((fields, starts, at + 1)),
                Some(b'\r') if bytes.get(at + 1) == Some(&b'\n') => {
                    return Ok((fields, starts, at + 2));
                }
                Some(b'\r') => return Err(bare_cr(at)),
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
                .position(|b| matches!(b, b',' | b'\n' | b'\r'))
                .map_or(bytes.len(), |n| at + n);
            match bytes.get(end) {
                Some(b',') => {
                    fields.push(text[at..end].to_string());
                    at = end + 1;
                }
                Some(b'\n') => {
                    fields.push(text[at..end].to_string());
                    return Ok((fields, starts, end + 1));
                }
                Some(_) if bytes.get(end + 1) == Some(&b'\n') => {
                    fields.push(text[at..end].to_string());
                    return Ok((fields, starts, end + 2));
                }
                Some(_) => return Err(bare_cr(end)),
                None => {
                    fields.push(text[at..end].to_string());
                    return Ok((fields, starts, end));
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
            [Value::string(malformed.why), Value::Int(at)],
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
    /// The byte at which each column's name starts.
    header_at: Vec<usize>,
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
            [Value::string(format!("line {line}: {why}")), Value::Int(at)],
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
        let (fields, starts, next) = record_at(text, at).map_err(|m| {
            let line = line + text[at..m.at].matches('\n').count();
            syntax_at(line, m.why, m.at)
        })?;
        // (A line with nothing on it: no field, not even "".)
        let blank = matches!(&text[at..next], "" | "\n" | "\r\n");
        records.push((fields, starts, line, at, blank));
        line += text[at..next].matches('\n').count();
        at = next;
    }
    let mut records = records.into_iter();
    let Some((header, header_at, _, _, _)) = records.next() else {
        return Ok(Table {
            header: Vec::new(),
            header_at: Vec::new(),
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
    let fields_of = |n: usize| match n {
        1 => "1 field".to_string(),
        n => format!("{n} fields"),
    };
    for (fields, _, line, at, blank) in records {
        // (A blank line is a row of one empty field: of a text of one
        // column, that is a row.)
        if blank && header.len() > 1 {
            let why = format!(
                "a blank line: every row has the header's {}",
                fields_of(header.len())
            );
            return Err(syntax_at(line, why, at));
        }
        if fields.len() != header.len() {
            let why = format!(
                "a row of {}, and the header has {}",
                fields_of(fields.len()),
                header.len()
            );
            return Err(syntax_at(line, why, at));
        }
        rows.push(fields);
        lines.push(line);
    }
    Ok(Table {
        header,
        header_at,
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
        Value::String(s, _) => Value::string(at_line(s, lines)),
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
        [Value::variant(bv::CSV_OTHER, [Value::string(message)])],
    )
}

static PARSE: Decoder = Decoder::new("csv.parse");

/// The fields of the record type `element`, each with its type at the
/// type's arguments.
fn record_fields(vm: &Vm, element: &FieldType) -> Vec<(String, FieldType)> {
    let (id, args) = match element {
        FieldType::Record(id) => (*id, &[][..]),
        FieldType::Applied(id, args) => (*id, args.as_slice()),
        _ => return Vec::new(),
    };
    let Some(info) = vm.runtime.type_info(id) else {
        return Vec::new();
    };
    (info.fields().iter())
        .map(|(name, field)| (name.clone(), field.instantiated(args).into_owned()))
        .collect()
}

/// `Err(Syntax(...))` of a column of the header of `table` that is the
/// name of a field of the record type `element` but for spaces around
/// it, where no column is the name itself: names are matched exactly.
fn spaced_column(vm: &Vm, table: &Table, element: &FieldType) -> Option<Value> {
    record_fields(vm, element)
        .iter()
        .filter(|(name, _)| !table.header.contains(name))
        .find_map(|(name, _)| {
            let k = (table.header.iter()).position(|cell| cell.trim() == name)?;
            let why = format!(
                "the header's column '{}' has spaces around it: column names are matched exactly",
                table.header[k]
            );
            Some(syntax_at(1, why, table.header_at[k]))
        })
}

/// The column the header of `table` lacks for a field of the record type
/// `element` that is no `Option`: a field with no column is missing from
/// every row, also where there is none.
fn missing_column(vm: &Vm, table: &Table, element: &FieldType) -> Option<String> {
    record_fields(vm, element)
        .into_iter()
        .find(|(name, field)| {
            !matches!(field, FieldType::Option(_)) && !table.header.contains(name)
        })
        .map(|(name, _)| name)
}

// ── Writing ─────────────────────────────────────────────────────────

/// `field` appended to `out` as RFC 4180 writes it: quoted when it
/// holds a comma, a quote or a line break (`\n` or `\r`), a quote in
/// it doubled; as it is otherwise.
fn write_field(out: &mut String, field: &str) {
    if field.contains([',', '"', '\n', '\r']) {
        out.push('"');
        out.push_str(&field.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(field);
    }
}

/// `fields` appended to `out` as one line, a comma between two, with
/// no line break at its end.
fn write_line<'f>(out: &mut String, fields: impl IntoIterator<Item = &'f str>) {
    for (k, field) in fields.into_iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        write_field(out, field);
    }
}

/// The text of a cell that `csv.parse` reads back as `data`, a field
/// of a row: empty for `None`, a number as silt shows it, a date or a
/// time as `json.stringify` writes it. (The checker lets no other kind
/// of field reach it.)
fn cell(data: &Data) -> Result<String, VmError> {
    Ok(match data {
        Data::Null => String::new(),
        Data::Int(n) => n.to_string(),
        Data::Float(f) => match crate::value::Float::new(*f) {
            Some(f) => f.to_string(),
            None => f.to_string(),
        },
        Data::Bool(b) => b.to_string(),
        Data::String(s) => s.to_string(),
        Data::Date(date) => date.to_string(),
        Data::Time(time) => time.to_string(),
        Data::DateTime(at) => format!("{}T{}", at.0, at.1),
        _ => return Err(unsound("csv.stringify", "rows")),
    })
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
        if let Some(spaced) = spaced_column(vm, &table, &element) {
            return Ok(Step::Done(spaced));
        }
        if let Some(name) = missing_column(vm, &table, &element) {
            return Ok(Step::Done(Value::variant(
                bv::ERR,
                [Value::variant(bv::CSV_MISSING_FIELD, [Value::string(name)])],
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

    // A header of the field names of the records' type, then a line
    // for each record, each line ended by a line break: what
    // `csv.parse` reads back as the same records.
    fn stringify(rows: typed::List) -> Result<String, VmError> {
        let mut out = String::new();
        for (k, row) in rows.iter().enumerate() {
            let Data::Table(fields) = data::data("csv.stringify", Format::Json, &row)? else {
                return Err(unsound("csv.stringify", "rows"));
            };
            if k == 0 {
                write_line(&mut out, fields.iter().map(|(name, _)| name.as_str()));
                out.push('\n');
            }
            let cells = (fields.iter())
                .map(|(_, data)| cell(data))
                .collect::<Result<Vec<String>, VmError>>()?;
            write_line(&mut out, cells.iter().map(String::as_str));
            out.push('\n');
            if out.len() > MAX_COLLECTION_LEN {
                return Err(too_long_text("csv.stringify", out.len()));
            }
        }
        Ok(out)
    }

    // The fields as one line, quoted where they need it: what
    // `split_line` reads back as the same fields.
    fn join_line(fields: typed::List) -> Result<String, VmError> {
        let mut texts = Vec::with_capacity(fields.len());
        for field in fields.iter() {
            let Value::String(text, _) = field else {
                return Err(unsound("csv.join_line", "fields"));
            };
            texts.push(text);
        }
        let mut out = String::new();
        write_line(&mut out, texts.iter().map(|text| &**text));
        if out.len() > MAX_COLLECTION_LEN {
            return Err(too_long_text("csv.join_line", out.len()));
        }
        Ok(out)
    }

    fn split_line(line: &str) -> Value {
        match fields_of_line(line) {
            Ok(fields) => ok(Value::list(
                fields.into_iter().map(Value::string).collect(),
            )),
            Err(malformed) => syntax_err(&malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AFTER_QUOTE, BARE_CR, Malformed, UNCLOSED, fields_of_line as split_line, record};

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
        // A carriage return in quotes is text.
        assert_eq!(fields("\"a\rb\""), ["a\rb"]);
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
        // A carriage return outside quotes that ends no line.
        assert_eq!(
            refused("a\rb"),
            Malformed {
                why: BARE_CR,
                at: 1
            }
        );
        assert_eq!(
            refused("a,b\r"),
            Malformed {
                why: BARE_CR,
                at: 3
            }
        );
        assert_eq!(
            refused("\"a\"\r"),
            Malformed {
                why: BARE_CR,
                at: 3
            }
        );
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
