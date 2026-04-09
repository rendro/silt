---
title: "io / fs"
section: "Standard Library"
order: 8
---

# io

Functions for file I/O, stdin, command-line arguments, and debug inspection.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `args` | `() -> List(String)` | Command-line arguments |
| `inspect` | `(a) -> String` | Debug representation of any value |
| `read_file` | `(String) -> Result(String, String)` | Read entire file as string |
| `read_line` | `() -> Result(String, String)` | Read one line from stdin |
| `write_file` | `(String, String) -> Result((), String)` | Write string to file |


## `io.args`

```
io.args() -> List(String)
```

Returns the command-line arguments as a list of strings, including the program
name.

```silt
fn main() {
    let args = io.args()
    list.each(args) { a -> println(a) }
}
```


## `io.inspect`

```
io.inspect(value: a) -> String
```

Returns a debug-style string representation of any value, using silt syntax
(e.g., strings include quotes, lists show brackets).

```silt
fn main() {
    let s = io.inspect([1, "hello", true])
    println(s)  // [1, "hello", true]
}
```


## `io.read_file`

```
io.read_file(path: String) -> Result(String, String)
```

Reads the entire contents of a file. Returns `Ok(contents)` on success or
`Err(message)` on failure. When called from a spawned task, the operation
transparently yields to the scheduler while the file is being read.

```silt
fn main() {
    match io.read_file("data.txt") {
        Ok(contents) -> println(contents)
        Err(e) -> println("Error: {e}")
    }
}
```


## `io.read_line`

```
io.read_line() -> Result(String, String)
```

Reads a single line from stdin (trailing newline stripped). Returns
`Ok(line)` on success or `Err(message)` on failure. When called from a
spawned task, the operation transparently yields to the scheduler.

```silt
fn main() {
    print("Name: ")
    match io.read_line() {
        Ok(name) -> println("Hello, {name}!")
        Err(e) -> println("Error: {e}")
    }
}
```


## `io.write_file`

```
io.write_file(path: String, contents: String) -> Result((), String)
```

Writes a string to a file, creating or overwriting it. Returns `Ok(())` on
success or `Err(message)` on failure. When called from a spawned task, the
operation transparently yields to the scheduler while the file is being
written.

```silt
fn main() {
    match io.write_file("output.txt", "hello") {
        Ok(_) -> println("written")
        Err(e) -> println("Error: {e}")
    }
}
```


---

# fs

Filesystem path queries.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `exists` | `(String) -> Bool` | Check if path exists |
| `is_file` | `(String) -> Bool` | Check if path is a file |
| `is_dir` | `(String) -> Bool` | Check if path is a directory |
| `list_dir` | `(String) -> Result(List(String), String)` | List entries in a directory |


## `fs.exists`

```
fs.exists(path: String) -> Bool
```

Returns `true` if the file or directory at `path` exists.

```silt
fn main() {
    when fs.exists("config.toml") -> println("found config")
    else -> println("no config")
}
```


## `fs.is_file`

```
fs.is_file(path: String) -> Bool
```

Returns `true` if the path exists and is a regular file.

```silt
fn main() {
    when fs.is_file("data.csv") -> println("it's a file")
    else -> println("not a file")
}
```


## `fs.is_dir`

```
fs.is_dir(path: String) -> Bool
```

Returns `true` if the path exists and is a directory.

```silt
fn main() {
    when fs.is_dir("src") -> println("it's a directory")
    else -> println("not a directory")
}
```


## `fs.list_dir`

```
fs.list_dir(path: String) -> List(String)
```

Returns a list of entry names in the given directory. Returns an error
if the path does not exist or is not a directory.

```silt
import fs

fn main() {
    let entries = fs.list_dir(".")
    list.each(entries, fn(name) { println(name) })
}
```
