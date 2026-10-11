---
title: "regex"
section: "Standard Library"
order: 9
---

# regex

Regular expression functions. Pattern strings use standard regex syntax.

Every function takes the text first and the pattern second, so a text pipes
into them (`line |> regex.split("\\s+")`), and returns a `Result`: a pattern
is text like any other, and one that is no pattern is `Err(RegexError)`, not
a failure of the program.

```silt
import regex

fn main() {
  match regex.find("abc 123", "(") {
    Ok(found) -> println(found)
    Err(regex.InvalidPattern(why, at)) -> println("{why} at {at}") -- unclosed group at 0
    Err(e) -> println(e.message())
  }
}
```

| Variant | Fields | Meaning |
|---------|--------|---------|
| `InvalidPattern(String, Int)` | what is wrong, the byte of the pattern where | the pattern does not parse |
| `TooBig` | — | the compiled pattern exceeds the size budget |
| `Other(String)` | message | unclassified failure |

With a pattern the program writes itself, the error cannot happen, and
`result.unwrap_or` says what the call is worth if it did:
`regex.is_match(name, "^[a-z]+$") |> result.unwrap_or(false)`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `captures` | `(String, String) -> Result(Option(List(String)), RegexError)` | Capture groups from first match |
| `captures_all` | `(String, String) -> Result(List(List(String)), RegexError)` | Capture groups from all matches |
| `captures_named` | `(String, String) -> Result(Option(Map(String, String)), RegexError)` | Named capture groups from first match |
| `find` | `(String, String) -> Result(Option(String), RegexError)` | First match |
| `find_all` | `(String, String) -> Result(List(String), RegexError)` | All matches |
| `is_match` | `(String, String) -> Result(Bool, RegexError)` | Test if pattern matches |
| `replace` | `(String, String, String) -> Result(String, RegexError)` | Replace first match |
| `replace_all` | `(String, String, String) -> Result(String, RegexError)` | Replace all matches |
| `replace_all_with` | `(String, String, Fn(String) -> String) -> Result(String, RegexError)` | Replace all with callback |
| `split` | `(String, String) -> Result(List(String), RegexError)` | Split on pattern |


## `regex.captures`

```
regex.captures(text: String, pattern: String) -> Result(Option(List(String)), RegexError)
```

Returns capture groups from the first match, or `None` if no match. The full
match is at index 0, followed by numbered groups.

```silt
import list
import regex

fn main() {
  match regex.captures("user@host", "(\\w+)@(\\w+)") {
    Ok(Some(groups)) -> {
      println(list.get(groups, 1)) -- Some(user)
      println(list.get(groups, 2)) -- Some(host)
    }
    Ok(None) -> println("no match")
    Err(e) -> println(e.message())
  }
}
```


## `regex.captures_all`

```
regex.captures_all(text: String, pattern: String) -> Result(List(List(String)), RegexError)
```

Returns capture groups for every match. Each inner list has the full match at
index 0 followed by numbered groups.

```silt
import regex

fn main() {
  println(regex.captures_all("1-2 and 3-4", "(\\d+)-(\\d+)"))
  -- Ok([[1-2, 1, 2], [3-4, 3, 4]])
}
```


## `regex.captures_named`

```
regex.captures_named(text: String, pattern: String) -> Result(Option(Map(String, String)), RegexError)
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

fn main() {
  match regex.captures_named("alice@example", "(?P<user>\\w+)@(?P<host>\\w+)") {
    Ok(Some(groups)) -> {
      println(map.get(groups, "user")) -- Some(alice)
      println(map.get(groups, "host")) -- Some(example)
    }
    Ok(None) -> println("no match")
    Err(e) -> println(e.message())
  }
}
```


## `regex.find`

```
regex.find(text: String, pattern: String) -> Result(Option(String), RegexError)
```

Returns `Some(matched_text)` for the first match, or `None`.

```silt
import regex

fn main() {
  println(regex.find("abc 123 def", "\\d+")) -- Ok(Some(123))
  println(regex.find("abc", "\\d+")) -- Ok(None)
}
```


## `regex.find_all`

```
regex.find_all(text: String, pattern: String) -> Result(List(String), RegexError)
```

Returns all non-overlapping matches as a list of strings.

```silt
import regex

fn main() {
  println(regex.find_all("a1 b22 c333", "\\d+")) -- Ok([1, 22, 333])
}
```


## `regex.is_match`

```
regex.is_match(text: String, pattern: String) -> Result(Bool, RegexError)
```

Returns `true` if the pattern matches anywhere in the text.

```silt
import regex

fn main() {
  println(regex.is_match("123", "^\\d+$")) -- Ok(true)
  println(regex.is_match("abc", "^\\d+$")) -- Ok(false)
}
```


## `regex.replace`

```
regex.replace(text: String, pattern: String, replacement: String) -> Result(String, RegexError)
```

Replaces the first match with the replacement string.

```silt
import regex

fn main() {
  println(regex.replace("abc 123 def 456", "\\d+", "NUM")) -- Ok(abc NUM def 456)
}
```


## `regex.replace_all`

```
regex.replace_all(text: String, pattern: String, replacement: String) -> Result(String, RegexError)
```

Replaces all matches with the replacement string.

```silt
import regex

fn main() {
  println(regex.replace_all("abc 123 def 456", "\\d+", "NUM")) -- Ok(abc NUM def NUM)
}
```


## `regex.replace_all_with`

```
regex.replace_all_with(text: String, pattern: String, f: Fn(String) -> String) -> Result(String, RegexError)
```

Replaces all matches by calling `f` with each matched text. The callback must
return a string.

```silt
import int
import regex
import result
import string

fn main() {
  let doubled = regex.replace_all_with("a1 b22 c333", "\\d+") { m ->
    let n = int.parse(m) |> result.unwrap_or(0)
    string.from(n * 2)
  }
  println(doubled) -- Ok(a2 b44 c666)
}
```


## `regex.split`

```
regex.split(text: String, pattern: String) -> Result(List(String), RegexError)
```

Splits the text on every occurrence of the pattern.

```silt
import regex
import result

fn main() {
  let parts = "hello   world   silt" |> regex.split("\\s+") |> result.unwrap_or([])
  println(parts) -- [hello, world, silt]
}
```
