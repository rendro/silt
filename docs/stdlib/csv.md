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
in two; `csv.split_line` does not, and `csv.parse` reads a whole text into
a list of records, as `json.parse` reads a document into a type. The other
way, `csv.stringify` writes a list of records as a text that `csv.parse`
reads back, and `csv.join_line` writes one line that `csv.split_line` reads
back, quoting what needs it.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `join_line` | `(List(String)) -> String` | The fields as one line: a field with a comma, a quote or a line break quoted, its quotes doubled |
| `parse` | `(String, type a) -> Result(List(a), CsvError)` | Every row of a text with a header as a record of the type, its fields by their columns' names |
| `split_line` | `(String) -> Result(List(String), CsvError)` | The fields of one line: quoted fields and `""` escapes as RFC 4180 writes them |
| `stringify` | `(List(a)) -> String` | A header of the field names, then a line for each record: the text `csv.parse` reads back |

## Errors

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Syntax(msg, offset)` | `String`, `Int` | Malformed CSV, `offset` being the 0-based byte of the text where it goes wrong: the opening quote of a quoted field that is not closed, the first byte after a closing quote that is no comma and no line end; for `csv.parse` also a row with another number of fields than the header (at the row's first byte) and a header that names a column twice. A message of `csv.parse` begins with the line (`line 3: ...`) |
| `TypeMismatch(expected, got)` | `String`, `String` | `csv.parse`: a cell that is no value of its field's type, `got` the cell and where (`'abc' at line 3, column age`) |
| `MissingField(name)` | `String` | `csv.parse`: the header has no column for a field that is no `Option` |
| `Other(msg)` | `String` | unclassified failure |

`CsvError` implements the built-in `Error` trait: `Error.message(e)` of a
`Syntax` error is `csv syntax error at byte 2: a quoted field is not closed`.

## `csv.join_line`

```
csv.join_line(fields: List(String)) -> String
```

The fields as one line, a comma between two and no line break at its end,
as RFC 4180 writes them: a field that holds a comma, a quote or a line
break (`\n` or `\r`) is quoted, and a quote in it doubled; any other field
is written as it is. `csv.split_line` reads the line back as the same
fields. An empty list and a list of one empty field are both the empty
line, which `csv.split_line` reads as `[""]`.

```silt
import csv

fn main() {
  println(csv.join_line(["1", "Smith, John", "say \"hi\""])) -- 1,"Smith, John","say ""hi"""
  println(csv.join_line(["a", "", "c"])) -- a,,c
  println(csv.split_line(csv.join_line(["a,b", "c"]))) -- Ok(["a,b", "c"])
}
```

## `csv.parse`

```
csv.parse(text: String, type a) -> Result(List(a), CsvError)
```

Reads a CSV text whose first line is a header (the names of the columns)
into a list of records of the type, one for each row after it, in order.
Each field of the record is read from the column of its name: the order of
the columns does not matter, and a column that no field names is left
alone. Records end at a line break outside quotes, `\n` or `\r\n`, and a
quoted field may hold line breaks. The one line break that ends the last
row ends the text: it is no empty row after it. A blank line, between rows
or at the end, is no row and is a `Syntax` error (`line 4: a blank line:
every row has the header's 3 fields`); in a text of one column it is a row
with an empty cell. A UTF-8 byte order mark before the header is dropped;
an empty text is `Ok([])`. Column names are matched exactly: a header cell
that is a field's name with spaces around it (` age`) is a `Syntax` error
that says so, `line 1: the header's column ' age' has spaces around it:
column names are matched exactly`, before a missing field is.

The rows are read by the decoder of `json.parse` and `toml.parse`, with a
cell's text read as its field's type:

| Function | Type argument |
|----------|---------------|
| `csv.parse` | a record type |

The fields of the record type may have these types:

- `Int`, `Float`, `String`, `Bool`
- `time.Date`, `time.Time`, `time.DateTime`
- `Option(T)` of these: an empty cell is `None`
- type aliases of these

A cell is text, taken as it is for a `String` (an empty one is `""`). An
`Int` and a `Float` are read as `int.parse` and `float.parse` read them
(spaces around the number are dropped), a `Bool` is `true` or `false`,
and a date or a time is written as `json.parse` reads one (`2024-03-09`,
`07:30:00`, `2024-03-09T07:30:00`). Anything else has no decoder: a list, a
map, a tuple, a record, an enum, `json.Value` or a type with a written
`json.Decode` impl in a field is a compile error, as is a type argument
that is no record type.

The errors: a field that is no `Option` and has no column is
`Err(MissingField(name))`, whatever the rows; a cell that is no value of
its field's type is `Err(TypeMismatch(type, cell))` with its line and
column (`csv type mismatch: expected Int, got 'abc' at line 3, column
age`, and `got an empty cell` for an empty one), a date that is none says
why as `json.parse` does; a row with another number of fields than the
header is a `Syntax` error (`line 4: a row of 3 fields, and the header has
5`, `a row of 1 field`), as is any text that `csv.split_line` refuses.

```silt
import csv
import list

type Person {
  name: String,
  age: Int,
  email: Option(String),
}

fn main() {
  let text = "name,age,email\n\"Smith, John\",42,john@example.com\nAda,36,\n"
  match csv.parse(text, Person) {
    Ok(people) -> {
      println(list.length(people)) -- 2
      list.each(people) { p -> println("{p.name} is {p.age}") }
    }
    Err(e) -> println(Error.message(e))
  }
  match csv.parse("name,age\nBob,old\n", Person) {
    Ok(_) -> println("read")
    Err(e) -> println(Error.message(e)) -- csv type mismatch: expected Int, got 'old' at line 2, column age
  }
}
```

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

A carriage return outside quotes is part of a line end, `\r\n`, or it is
a `Syntax` error (RFC 4180's text has none): `a,b\r` and `"a","b"\r` are
refused, `a carriage return (\r) outside quotes: csv lines end in \n or
\r\n`. That is what a line of a file with CRLF line ends holds when the
text is cut with `string.split(text, "\n")`: cut it with `string.lines`,
which takes `\r\n` as one line end, or give the whole text to `csv.parse`.
Inside quotes a carriage return is text, as a line break is.

```silt
import csv

fn main() {
  println(csv.split_line("1,\"Smith, John\",\"say \"\"hi\"\"\"")) -- Ok(["1", "Smith, John", "say \"hi\""])
  println(csv.split_line("a,,c")) -- Ok(["a", "", "c"])
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


## `csv.stringify`

```
csv.stringify(rows: List(a)) -> String
```

Writes a list of records as a CSV text that `csv.parse` reads back: a
header of the field names of the records' type, in the order the type
declares them, then one line for each record, in order. Every line ends
in `\n`, and a field is quoted as `csv.join_line` quotes it. An empty list
is the empty text, which `csv.parse` reads as `Ok([])`.

The records' type is checked as `csv.parse`'s type argument is: a record
type whose fields are `Int`, `Float`, `String`, `Bool`, `time.Date`,
`time.Time`, `time.DateTime`, an `Option` of these or an alias of these.
Anything else is a compile error (`csv.stringify cannot write ...`).

A cell is written as `csv.parse` reads it: a number as silt shows it
(`42`, `1.5`), a `Bool` as `true` or `false`, a date or a time as
`json.stringify` writes one (`2024-03-09`), a `String` as it is, and `None`
as an empty cell. So `Some("")` of an `Option(String)` is an empty cell
too, which `csv.parse` reads as `None`.

```silt
import csv
import list

type Person {
  name: String,
  age: Int,
  email: Option(String),
}

fn main() {
  let people = [
    Person { name: "Smith, John", age: 42, email: Some("john@example.com") },
    Person { name: "Ada", age: 36, email: None },
  ]
  let text = csv.stringify(people)
  print(text)
  -- name,age,email
  -- "Smith, John",42,john@example.com
  -- Ada,36,
  println(csv.parse(text, Person) == Ok(people)) -- true
}
```
