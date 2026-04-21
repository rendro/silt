---
title: "regex"
section: "Standard Library"
order: 9
---

# regex

Regular expression functions. Pattern strings use standard regex syntax.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `captures` | `(String, String) -> Option(List(String))` | Capture groups from first match |
| `captures_all` | `(String, String) -> List(List(String))` | Capture groups from all matches |
| `captures_named` | `(String, String) -> Option(Map(String, String))` | Named capture groups from first match |
| `find` | `(String, String) -> Option(String)` | First match |
| `find_all` | `(String, String) -> List(String)` | All matches |
| `is_match` | `(String, String) -> Bool` | Test if pattern matches |
| `replace` | `(String, String, String) -> String` | Replace first match |
| `replace_all` | `(String, String, String) -> String` | Replace all matches |
| `replace_all_with` | `(String, String, (String) -> String) -> String` | Replace all with callback |
| `split` | `(String, String) -> List(String)` | Split on pattern |


## `regex.captures`

```
regex.captures(pattern: String, text: String) -> Option(List(String))
```

Returns capture groups from the first match, or `None` if no match. The full
match is at index 0, followed by numbered groups.

```silt
import regex
import list
fn main() {
    match regex.captures("(\\w+)@(\\w+)", "user@host") {
        Some(groups) -> {
            println(list.get(groups, 1))  -- Some("user")
            println(list.get(groups, 2))  -- Some("host")
        }
        None -> println("no match")
    }
}
```


## `regex.captures_all`

```
regex.captures_all(pattern: String, text: String) -> List(List(String))
```

Returns capture groups for every match. Each inner list has the full match at
index 0 followed by numbered groups.

```silt
import regex
fn main() {
    let results = regex.captures_all("(\\d+)-(\\d+)", "1-2 and 3-4")
    -- [["1-2", "1", "2"], ["3-4", "3", "4"]]
}
```


## `regex.captures_named`

```
regex.captures_named(pattern: String, text: String) -> Option(Map(String, String))
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
import regex
import map
fn main() {
    match regex.captures_named("(?P<user>\\w+)@(?P<host>\\w+)", "alice@example") {
        Some(groups) -> {
            println(map.get(groups, "user"))  -- Some("alice")
            println(map.get(groups, "host"))  -- Some("example")
        }
        None -> println("no match")
    }
}
```


## `regex.find`

```
regex.find(pattern: String, text: String) -> Option(String)
```

Returns `Some(matched_text)` for the first match, or `None`.

```silt
import regex
fn main() {
    let first = regex.find("\\d+", "abc 123 def")
    println(first)  -- Some("123")
}
```


## `regex.find_all`

```
regex.find_all(pattern: String, text: String) -> List(String)
```

Returns all non-overlapping matches as a list of strings.

```silt
import regex
fn main() {
    let nums = regex.find_all("\\d+", "a1 b22 c333")
    println(nums)  -- ["1", "22", "333"]
}
```


## `regex.is_match`

```
regex.is_match(pattern: String, text: String) -> Bool
```

Returns `true` if the pattern matches anywhere in the text.

```silt
import regex
fn main() {
    println(regex.is_match("^\\d+$", "123"))    -- true
    println(regex.is_match("^\\d+$", "abc"))    -- false
}
```


## `regex.replace`

```
regex.replace(pattern: String, text: String, replacement: String) -> String
```

Replaces the first match with the replacement string.

```silt
import regex
fn main() {
    let replaced = regex.replace("\\d+", "abc 123 def 456", "NUM")
    println(replaced)  -- "abc NUM def 456"
}
```


## `regex.replace_all`

```
regex.replace_all(pattern: String, text: String, replacement: String) -> String
```

Replaces all matches with the replacement string.

```silt
import regex
fn main() {
    let scrubbed = regex.replace_all("\\d+", "abc 123 def 456", "NUM")
    println(scrubbed)  -- "abc NUM def NUM"
}
```


## `regex.replace_all_with`

```
regex.replace_all_with(pattern: String, text: String, f: (String) -> String) -> String
```

Replaces all matches by calling `f` with each matched text. The callback must
return a string.

```silt
import int
import result

import regex
fn main() {
    let doubled = regex.replace_all_with("\\d+", "a1 b22 c333") { m ->
        int.to_string(int.parse(m) |> result.unwrap_or(0) |> fn(n) { n * 2 })
    }
    println(doubled)  -- "a2 b44 c666"
}
```


## `regex.split`

```
regex.split(pattern: String, text: String) -> List(String)
```

Splits the text on every occurrence of the pattern.

```silt
import regex
fn main() {
    let parts = regex.split("\\s+", "hello   world   silt")
    println(parts)  -- ["hello", "world", "silt"]
}
```
