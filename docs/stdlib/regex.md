---
title: "regex"
---

# regex

Regular expression functions. Pattern strings use standard regex syntax.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `captures` | `(String, String) -> Option(List(String))` | Capture groups from first match |
| `captures_all` | `(String, String) -> List(List(String))` | Capture groups from all matches |
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
fn main() {
    match regex.captures("(\\w+)@(\\w+)", "user@host") {
        Some(groups) -> {
            println(list.get(groups, 1))  // Some("user")
            println(list.get(groups, 2))  // Some("host")
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
fn main() {
    let results = regex.captures_all("(\\d+)-(\\d+)", "1-2 and 3-4")
    // [["1-2", "1", "2"], ["3-4", "3", "4"]]
}
```


## `regex.find`

```
regex.find(pattern: String, text: String) -> Option(String)
```

Returns `Some(matched_text)` for the first match, or `None`.

```silt
fn main() {
    let result = regex.find("\\d+", "abc 123 def")
    println(result)  // Some("123")
}
```


## `regex.find_all`

```
regex.find_all(pattern: String, text: String) -> List(String)
```

Returns all non-overlapping matches as a list of strings.

```silt
fn main() {
    let nums = regex.find_all("\\d+", "a1 b22 c333")
    println(nums)  // ["1", "22", "333"]
}
```


## `regex.is_match`

```
regex.is_match(pattern: String, text: String) -> Bool
```

Returns `true` if the pattern matches anywhere in the text.

```silt
fn main() {
    println(regex.is_match("^\\d+$", "123"))    // true
    println(regex.is_match("^\\d+$", "abc"))    // false
}
```


## `regex.replace`

```
regex.replace(pattern: String, text: String, replacement: String) -> String
```

Replaces the first match with the replacement string.

```silt
fn main() {
    let result = regex.replace("\\d+", "abc 123 def 456", "NUM")
    println(result)  // "abc NUM def 456"
}
```


## `regex.replace_all`

```
regex.replace_all(pattern: String, text: String, replacement: String) -> String
```

Replaces all matches with the replacement string.

```silt
fn main() {
    let result = regex.replace_all("\\d+", "abc 123 def 456", "NUM")
    println(result)  // "abc NUM def NUM"
}
```


## `regex.replace_all_with`

```
regex.replace_all_with(pattern: String, text: String, f: (String) -> String) -> String
```

Replaces all matches by calling `f` with each matched text. The callback must
return a string.

```silt
fn main() {
    let result = regex.replace_all_with("\\d+", "a1 b22 c333") { m ->
        int.to_string(int.parse(m) |> result.unwrap_or(0) |> fn(n) { n * 2 })
    }
    // "a2 b44 c666"
}
```


## `regex.split`

```
regex.split(pattern: String, text: String) -> List(String)
```

Splits the text on every occurrence of the pattern.

```silt
fn main() {
    let parts = regex.split("\\s+", "hello   world   silt")
    println(parts)  // ["hello", "world", "silt"]
}
```
