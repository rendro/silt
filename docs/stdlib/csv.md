---
title: "csv"
section: "Standard Library"
order: 26
---

# csv

Comma-separated values, as [RFC 4180](https://www.rfc-editor.org/rfc/rfc4180)
writes them. Fields are separated by commas, records end at a line break
(`\n` or `\r\n`). A field that holds a comma, a quote or a line break is
quoted, `"Smith, John"`, and a quote inside a quoted field is doubled:
`"say ""hi"""` is `say "hi"`. `string.split(line, ",")` breaks such a field
in two; `csv.split_line` does not.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `split_line` | `(String) -> Result(List(String), CsvError)` | The fields of one line: quoted fields and `""` escapes as RFC 4180 writes them |

## Errors

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(msg, offset)` | `String`, `Int` | Malformed CSV, `offset` being the 0-based byte of the text where it goes wrong: the opening quote of a quoted field that is not closed, the first byte after a closing quote that is no comma and no line end |
| `TypeMismatch(expected, got)` | `String`, `String` | reserved for the decoding of a whole text |
| `MissingField(name)` | `String` | reserved for the decoding of a whole text |
| `Other(msg)` | `String` | unclassified failure |

`CsvError` implements the built-in `Error` trait: `Error.message(e)` of a
`Syntax` error is `csv syntax error at byte 2: a quoted field is not closed`.

## `csv.split_line`

```
csv.split_line(line: String) -> Result(List(String), CsvError)
```

The fields of one line, in order. A field is taken as it is written:
nothing is trimmed, an empty line is one empty field (`[""]`), and a field
that starts with a quote is read up to the quote that closes it, with `""`
read as one quote and commas and line breaks inside it kept. A quote in a
field that does not start with one is part of the field (`5" disk`), as
every CSV reader in use takes it.

One line break at the end of the line (`\n` or `\r\n`, as a line read from
a file may keep) is not part of it. A line break outside quotes anywhere
else is a `Syntax` error, as are a quoted field that is not closed and text
after a closing quote before the next comma (`"a"b`).

```silt
import csv

fn main() {
  println(csv.split_line("1,\"Smith, John\",\"say \"\"hi\"\"\"")) -- Ok([1, Smith, John, say "hi"])
  println(csv.split_line("a,,c")) -- Ok([a, , c])
  match csv.split_line("a,\"bc") {
    Ok(fields) -> println(fields)
    Err(e) -> println(Error.message(e)) -- csv syntax error at byte 2: a quoted field is not closed
  }
}
```

A file of lines whose fields hold no line break is read line by line:

```silt
import csv
import io
import list
import string

fn main() {
  match io.read_file("people.csv") {
    Ok(text) -> {
      let rows = string.lines(text) |> list.map { line -> csv.split_line(line) }
      println(list.length(rows))
    }
    Err(e) -> println(Error.message(e))
  }
}
```
