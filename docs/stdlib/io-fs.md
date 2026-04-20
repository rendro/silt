---
title: "io / fs / env"
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
import io
import list
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
import io
fn main() {
    let s = io.inspect((1, "hello", true))
    println(s)  -- (1, "hello", true)
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
import io
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
import io
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
import io
fn main() {
    match io.write_file("output.txt", "hello") {
        Ok(_) -> println("written")
        Err(e) -> println("Error: {e}")
    }
}
```


---

# env

Environment variable access. Requires `import env`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `get` | `(String) -> Option(String)` | Read an environment variable |
| `set` | `(String, String) -> ()` | Set an environment variable |


## `env.get`

```
env.get(key: String) -> Option(String)
```

Returns `Some(value)` if the environment variable `key` is set, or `None`
if it is not.

```silt
import env

fn main() {
    match env.get("HOME") {
        Some(home) -> println("Home directory: {home}")
        None -> println("HOME not set")
    }
}
```


## `env.set`

```
env.set(key: String, value: String) -> ()
```

Sets the environment variable `key` to `value` for the current process.

```silt
import env

fn main() {
    env.set("MY_VAR", "hello")
    println(env.get("MY_VAR"))  -- Some("hello")
}
```


---

# fs

Filesystem operations: queries, directory management, and file manipulation.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `copy` | `(String, String) -> Result((), String)` | Copy a file |
| `exists` | `(String) -> Bool` | Check if path exists |
| `glob` | `(String) -> Result(List(String), String)` | Match paths by glob pattern |
| `is_dir` | `(String) -> Bool` | Check if path is a directory |
| `is_file` | `(String) -> Bool` | Check if path is a file |
| `is_symlink` | `(String) -> Bool` | Check if path is a symlink (without following) |
| `list_dir` | `(String) -> Result(List(String), String)` | List entries in a directory |
| `mkdir` | `(String) -> Result((), String)` | Create a directory (and parents) |
| `read_link` | `(String) -> Result(String, String)` | Read a symlink's target (without following) |
| `remove` | `(String) -> Result((), String)` | Remove a file or empty directory |
| `rename` | `(String, String) -> Result((), String)` | Rename / move a file or directory |
| `stat` | `(String) -> Result(FileStat, String)` | Fetch filesystem metadata for a path |
| `walk` | `(String) -> Result(List(String), String)` | Recursively list all paths under a directory |


## `fs.copy`

```
fs.copy(from: String, to: String) -> Result((), String)
```

Copies a file from `from` to `to`. Returns `Ok(())` on success or
`Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.copy("original.txt", "backup.txt") {
        Ok(_) -> println("copied")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.exists`

```
fs.exists(path: String) -> Bool
```

Returns `true` if the file or directory at `path` exists.

```silt
import fs
fn main() {
    match {
        fs.exists("config.toml") -> println("found config")
        _ -> println("no config")
    }
}
```


## `fs.is_file`

```
fs.is_file(path: String) -> Bool
```

Returns `true` if the path exists and is a regular file.

```silt
import fs
fn main() {
    match {
        fs.is_file("data.csv") -> println("it's a file")
        _ -> println("not a file")
    }
}
```


## `fs.is_dir`

```
fs.is_dir(path: String) -> Bool
```

Returns `true` if the path exists and is a directory.

```silt
import fs
fn main() {
    match {
        fs.is_dir("src") -> println("it's a directory")
        _ -> println("not a directory")
    }
}
```


## `fs.list_dir`

```
fs.list_dir(path: String) -> Result(List(String), String)
```

Returns `Ok(entries)` with a list of entry names in the given directory,
or `Err(message)` if the path does not exist or is not a directory.

```silt
import fs

import list
fn main() {
    match fs.list_dir(".") {
        Ok(entries) -> list.each(entries) { name -> println(name) }
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.mkdir`

```
fs.mkdir(path: String) -> Result((), String)
```

Creates a directory at `path`, including any missing parent directories.
Returns `Ok(())` on success or `Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.mkdir("output/reports") {
        Ok(_) -> println("directory created")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.remove`

```
fs.remove(path: String) -> Result((), String)
```

Removes a file or an empty directory. Returns `Ok(())` on success or
`Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.remove("temp.txt") {
        Ok(_) -> println("removed")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.rename`

```
fs.rename(from: String, to: String) -> Result((), String)
```

Renames (moves) a file or directory from `from` to `to`. Returns `Ok(())`
on success or `Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.rename("old_name.txt", "new_name.txt") {
        Ok(_) -> println("renamed")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.stat`

```
fs.stat(path: String) -> Result(FileStat, String)

record FileStat {
    size: Int,        // size in bytes
    is_file: Bool,
    is_dir: Bool,
    is_symlink: Bool, // true if the path itself is a symlink
    modified: Int,    // modified time, unix seconds (0 if unsupported)
    readonly: Bool,   // true if the OS reports the path as read-only
}
```

Returns filesystem metadata for `path`. The stat is taken on the path
itself (via `symlink_metadata`): if the path is a symlink, `is_symlink`
is `true` and `is_file` / `is_dir` both report on the link rather than
its target. To stat the target, call `fs.read_link` and then `fs.stat`
on the result.

Returns `Err(message)` when the path does not exist, permission is
denied, or the OS reports another I/O error.

```silt
import fs

fn main() {
    match fs.stat("README.md") {
        Ok(s) -> println("size = {s.size}, modified = {s.modified}")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.is_symlink`

```
fs.is_symlink(path: String) -> Bool
```

Returns `true` if `path` names a symlink. Does **not** follow the
symlink (unlike `fs.is_file` / `fs.is_dir`, which follow). Returns
`false` if the path does not exist.

```silt
import fs
fn main() {
    match {
        fs.is_symlink("link") -> println("it's a symlink")
        _ -> println("not a symlink")
    }
}
```


## `fs.read_link`

```
fs.read_link(path: String) -> Result(String, String)
```

Returns the raw target of a symlink (the value it points at, not the
resolved destination). Errors when `path` is not a symlink or cannot
be read.

```silt
import fs

fn main() {
    match fs.read_link("link") {
        Ok(target) -> println("points at {target}")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.walk`

```
fs.walk(root: String) -> Result(List(String), String)
```

Recursively walks the directory tree rooted at `root` and returns a
flat list of every path discovered — files **and** directories, in
arbitrary order. Paths are canonicalized when possible (so the result
is safe to pass back into other `fs.*` calls without worrying about
cwd drift); when canonicalization fails — e.g. the entry was removed
between the walk and the canonicalize — the raw path is preserved.

**Symlinks are not followed.** This avoids infinite loops on cyclic
trees (including trees with a `link -> .` inside them) and matches the
principle of least surprise for build tooling. If you need to follow
symlinks, walk, then post-filter with `fs.stat` / `fs.read_link`.

**Entry cap.** To avoid accidental OOM on huge trees, `fs.walk`
refuses to materialize more than `1_000_000` entries. Hitting the cap
returns `Err("fs.walk: exceeded 1000000 entries (cap)")` rather than
silently truncating — callers can then narrow the root or paginate at
a higher layer.

```silt
import fs

fn main() {
    match fs.walk("src") {
        Ok(paths) -> println("{paths}")
        Err(e) -> println("Error: {e}")
    }
}
```


## `fs.glob`

```
fs.glob(pattern: String) -> Result(List(String), String)
```

Returns the list of paths matching a Unix-style glob `pattern`.
Patterns are anchored at the current working directory unless they
start with `/` (or a drive prefix on Windows). Syntax mirrors the
`glob` crate:

- `*` matches any sequence except `/`
- `?` matches a single character
- `[abc]` / `[!abc]` matches one of a character set
- `**` matches any number of directories recursively

Returns `Err(message)` if the pattern itself is malformed. The result
is subject to the same `1_000_000`-entry cap as `fs.walk`.

```silt
import fs

fn main() {
    match fs.glob("src/**/*.silt") {
        Ok(files) -> println("{files}")
        Err(e) -> println("Error: {e}")
    }
}
```
