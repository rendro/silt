---
title: "regex"
section: "Standard Library"
order: 9
---

# regex

Regular expression functions. Pattern strings use standard regex syntax.

A pattern is checked once, by `regex.compile`, which gives a `Regex` or
`Err(RegexError)`. Every other function takes the text first and the `Regex`
second, so a text pipes into them (`line |> regex.split(spaces)`), and gives
its value bare: a `Bool`, an `Option`, a `List`, a `String`. A pattern can
only be wrong where it is compiled, so a closure that calls `regex.is_match`
has no error to deal with.

Write a pattern as a raw string, `"""\d{2,5}"""`, when it holds a `{` or a
`\`: in a `"..."` string a `{` starts an interpolation and a `\` an escape,
so the same pattern there is `"\\d\{2,5}"`. A raw string has neither, and
holds the pattern as the regex reads it.

```silt
import list
import regex

fn main() -> Result((), regex.RegexError) {
  let port = regex.compile("""\d{2,5}""")?
  println(regex.is_match("port 8080", port)) -- true
  let lines = ["port 80", "no port", "port 8080"]
  println(list.filter(lines) { l -> regex.is_match(l, port) }) -- ["port 80", "port 8080"]
  Ok(())
}
```

```silt
import regex

fn main() {
  match regex.compile("(") {
    Ok(_) -> println("compiles")
    Err(regex.InvalidPattern(why, at)) -> println("{why} at {at}") -- unclosed group at 0
    Err(e) -> println(e)
  }
}
```

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidPattern(String, Int)` | what is wrong, and the character of the pattern where: counted from 0, as `string.slice` counts. It is the pattern's length when the fault is that the pattern ends there | the pattern does not parse |
| `TooBig` | — | the compiled pattern exceeds the size budget |

A pattern compiles to at most 4 MiB; a larger one is `TooBig`: the pattern
is valid, but too large to run. What grows is a class of characters repeated
many times, the more so for a Unicode class: `^.{8,4096}$` (8 to 4096
characters of any kind) and `\w{100}` are `TooBig`; `^\w{3,32}$` (about
1.8 MB) is not, and the ASCII forms `[a-z0-9_]{100}` and `(?-u)\w{100}` are
small. A length check is better written with `string.length`.

Two `Regex` values of the same pattern are equal (`==`). A `Regex` is shown
as its pattern, the text `==` compares: `"{re}"` and `println(re)` write
`\d+` for the pattern `\d+`, also inside a list, a record or a `Result`,
as a value whose type has a `Display` impl is. `io.inspect` and the message
of a failed `test.assert_eq` write it as `<regex:PATTERN>`, so that it does
not read as a `String` there.

```silt
import io
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("""\d+""")?
  println(digits) -- \d+
  println("pattern: {digits}") -- pattern: \d+
  println([digits]) -- [\d+]
  println(io.inspect(digits)) -- <regex:\d+>
  Ok(())
}
```

A compiled pattern is kept for the next `regex.compile` of the same pattern,
by every task of the program: up to 256 patterns and 128 MiB of them. When a
new one does not fit, the patterns used least recently are dropped from that
store, and compiled again if they are compiled again; a `Regex` the program
holds stays as it is. A pattern that does not compile is remembered too, so
the error comes back without compiling it again. What a search keeps for its
next one, on each thread that searched with a pattern, is not counted in the
128 MiB: it grows with the searching, up to a few MiB a pattern and thread.

The 256 patterns and 128 MiB bound that store only. A `Regex` the program
keeps, in a list, a map or a record, is not counted: each one holds its
compiled pattern, at most 4 MiB, for as long as the program holds it, and
nothing limits how many the program keeps, as nothing limits how many strings
it keeps. A program that compiles patterns given by its users should cap how
many of them it keeps.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `captures` | `(String, Regex) -> Option(List(String))` | Capture groups from first match |
| `captures_all` | `(String, Regex) -> List(List(String))` | Capture groups from all matches |
| `captures_named` | `(String, Regex) -> Option(Map(String, String))` | Named capture groups from first match |
| `compile` | `(String) -> Result(Regex, RegexError)` | Check a pattern and compile it, once |
| `escape` | `(String) -> String` | The text as a pattern that matches it and nothing else |
| `find` | `(String, Regex) -> Option(String)` | First match |
| `find_all` | `(String, Regex) -> List(String)` | All matches |
| `find_all_at` | `(String, Regex) -> List((Int, Int))` | Where each match is: (start, end) in characters |
| `find_at` | `(String, Regex) -> Option((Int, Int))` | Where the first match is: (start, end) in characters |
| `is_match` | `(String, Regex) -> Bool` | Test if pattern matches |
| `replace` | `(String, Regex, String) -> String` | Replace all matches |
| `replace_first` | `(String, Regex, String) -> String` | Replace first match |
| `replace_with` | `(String, Regex, Fn(String) -> String) -> String` | Replace all with callback |
| `split` | `(String, Regex) -> List(String)` | Split on pattern |


## `regex.captures`

```
regex.captures(text: String, re: Regex) -> Option(List(String))
```

Returns capture groups from the first match, or `None` if no match. The full
match is at index 0, followed by numbered groups. A group that took no part
in the match (`(a)?` where there is no `a`) is the empty string, as one that
matched nothing is: `regex.captures("b", re)` of the pattern `(a)?(b)` is
`Some(["b", "", "b"])`. `regex.captures_named` leaves such a group out.

```silt
import list
import regex

fn main() -> Result((), regex.RegexError) {
  let address = regex.compile("(\\w+)@(\\w+)")?
  match regex.captures("user@host", address) {
    Some(groups) -> {
      println(list.get(groups, 1)) -- Some("user")
      println(list.get(groups, 2)) -- Some("host")
    }
    None -> println("no match")
  }
  Ok(())
}
```


## `regex.captures_all`

```
regex.captures_all(text: String, re: Regex) -> List(List(String))
```

Returns capture groups for every match. Each inner list has the full match at
index 0 followed by numbered groups.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let pair = regex.compile("(\\d+)-(\\d+)")?
  println(regex.captures_all("1-2 and 3-4", pair))
  -- [["1-2", "1", "2"], ["3-4", "3", "4"]]
  Ok(())
}
```


## `regex.captures_named`

```
regex.captures_named(text: String, re: Regex) -> Option(Map(String, String))
```

Returns a map of named capture groups from the first match. Named groups use
the `(?P<name>...)` syntax.

- Returns `None` if the pattern has no named groups or if it does not match.
- A named group that is present in the pattern but did not participate in the
  match (e.g. inside an optional `(...)?`) is **omitted** from the map — it is
  not mapped to `""`.
- Unnamed numbered groups are ignored; use `regex.captures` for positional
  access.

```silt
import map
import regex

fn main() -> Result((), regex.RegexError) {
  let address = regex.compile("(?P<user>\\w+)@(?P<host>\\w+)")?
  match regex.captures_named("alice@example", address) {
    Some(groups) -> {
      println(map.get(groups, "user")) -- Some("alice")
      println(map.get(groups, "host")) -- Some("example")
    }
    None -> println("no match")
  }
  Ok(())
}
```


## `regex.compile`

```
regex.compile(pattern: String) -> Result(Regex, RegexError)
```

Checks the pattern and compiles it: `Ok` of the `Regex` that every other
function takes, or `Err(InvalidPattern(why, at))` for a pattern that does
not parse and `Err(TooBig)` for one too large to run. Compile a pattern once
and pass the `Regex` on; `?` gives the error to the caller.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  println(regex.find_all("a1 b22", digits)) -- ["1", "22"]
  match regex.compile("a(b") {
    Ok(_) -> println("compiles")
    Err(e) -> println(e) -- invalid regex pattern at position 1: unclosed group
  }
  Ok(())
}
```


## `regex.escape`

```
regex.escape(text: String) -> String
```

The text with a `\` in front of every character that means something in a
pattern (`.`, `*`, `(`, `[`, `{`, `$` and the others), so the result, as a
pattern, matches the text and nothing else. Escape text a user gave before
it goes into a pattern: a `.` in it would match any character, and a `(`
would make the pattern fail to compile.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  println(regex.escape("1.5 (approx)")) -- 1\.5 \(approx\)
  let price = regex.compile("^{regex.escape("$1.50")}$")?
  println(regex.is_match("$1.50", price)) -- true
  println(regex.is_match("$1x50", price)) -- false
  Ok(())
}
```


## `regex.find`

```
regex.find(text: String, re: Regex) -> Option(String)
```

The first match of `re` in `text`, or `None`.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  println(regex.find("a1 b22", digits)) -- Some("1")
  println(regex.find("none", digits)) -- None
  Ok(())
}
```


## `regex.find_all`

```
regex.find_all(text: String, re: Regex) -> List(String)
```

Returns all non-overlapping matches as a list of strings.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  println(regex.find_all("a1 b22 c333", digits)) -- ["1", "22", "333"]
  Ok(())
}
```


## `regex.find_all_at`

```
regex.find_all_at(text: String, re: Regex) -> List((Int, Int))
```

Where each match of `re` in `text` is, in order, as `regex.find_at` gives
the first one: `(start, end)` in characters, the end not in the match. A
match of nothing is `(n, n)`, at a place between two characters.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let word = regex.compile("""\bab""")?
  println(regex.find_all_at("ab ab", word)) -- [(0, 2), (3, 5)]
  let digits = regex.compile("""\d+""")?
  println(regex.find_all_at("é1 ü22", digits)) -- [(1, 2), (4, 6)]
  Ok(())
}
```


## `regex.find_at`

```
regex.find_at(text: String, re: Regex) -> Option((Int, Int))
```

Where the first match of `re` in `text` is: `Some((start, end))`, or `None`.
Both are counted in characters from 0, as `string.slice` and
`string.index_of` count, and the end is the first character after the
match, so `string.slice(text, start, end)` is the match. `string.index_of`
of the matched text finds where that text first occurs, which is not always
the match: in `"cab ab"`, `\bab` (an `ab` at the start of a word) matches at
4, and `string.index_of` finds the `ab` of `cab` at 1.

```silt
import regex
import string

fn main() -> Result((), regex.RegexError) {
  let text = "cab ab"
  let word = regex.compile("""\bab""")?
  println(regex.find_at(text, word)) -- Some((4, 6))
  println(string.index_of(text, "ab")) -- Some(1)
  match regex.find_at(text, word) {
    Some((start, end)) -> println(string.slice(text, start, end)) -- ab
    None -> println("no match")
  }
  println(regex.find_at("none", word)) -- None
  Ok(())
}
```


## `regex.is_match`

```
regex.is_match(text: String, re: Regex) -> Bool
```

Returns `true` if the pattern matches anywhere in the text.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let number = regex.compile("^\\d+$")?
  println(regex.is_match("123", number)) -- true
  println(regex.is_match("abc", number)) -- false
  Ok(())
}
```


## `regex.replace`

```
regex.replace(text: String, re: Regex, replacement: String) -> String
```

Replaces every match with the replacement string, as `string.replace`
replaces every occurrence. In the replacement, `$1` stands for the text of
group 1, `$name` for the text of a named group, `$0` for the whole match,
and `$$` for a `$`. A name runs as far as letters, digits and `_` go, so
`$1x` is the group named `1x`, which is no group and stands for nothing:
write `${1}x`, which a string literal writes `"$\{1}x"` (a `{` in it starts
an interpolation). A group that took no part in the match stands for
nothing. With `re` the pattern `(a)(b)`, `regex.replace("ab", re, "$2$1")` is
`"ba"`.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  println(regex.replace("abc 123 def 456", digits, "NUM")) -- abc NUM def NUM
  Ok(())
}
```


## `regex.replace_first`

```
regex.replace_first(text: String, re: Regex, replacement: String) -> String
```

Replaces the first match only, with the replacement string, which reads
`$1`, `$name` and `$$` as in `regex.replace`.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  println(regex.replace_first("abc 123 def 456", digits, "NUM")) -- abc NUM def 456
  Ok(())
}
```


## `regex.replace_with`

```
regex.replace_with(text: String, re: Regex, f: Fn(String) -> String) -> String
```

Replaces all matches by calling `f` with each matched text. The callback must
return a string.

```silt
import int
import regex
import result
import string

fn main() -> Result((), regex.RegexError) {
  let digits = regex.compile("\\d+")?
  let doubled = regex.replace_with("a1 b22 c333", digits) { m ->
    let n = int.parse(m) |> result.unwrap_or(0)
    string.from(n * 2)
  }
  println(doubled) -- a2 b44 c666
  Ok(())
}
```


## `regex.split`

```
regex.split(text: String, re: Regex) -> List(String)
```

Splits the text on every occurrence of the pattern. A match of nothing at
the start or the end of the text gives no empty piece there, as
`string.split` with an empty separator: with `nothing` the pattern `""`,
`regex.split("abc", nothing)` is `["a", "b", "c"]`, and `regex.split("",
nothing)` is `[]`. A match of something there does: with `comma` the pattern
`","`, `regex.split(",a,", comma)` is `["", "a", ""]`.

```silt
import regex

fn main() -> Result((), regex.RegexError) {
  let spaces = regex.compile("\\s+")?
  let parts = "hello   world   silt" |> regex.split(spaces)
  println(parts) -- ["hello", "world", "silt"]
  Ok(())
}
```
