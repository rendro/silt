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
| `read_file` | `(String) -> Result(String, IoError)` | Read entire file as string |
| `read_line` | `() -> Result(String, IoError)` | Read one line from stdin |
| `write_file` | `(String, String) -> Result((), IoError)` | Write string to file |

## Errors

Every fallible `io` / `fs` function returns `Result(T, IoError)`.

| Variant                      | Meaning                                    |
|------------------------------|--------------------------------------------|
| `IoNotFound(path)`           | file or directory does not exist           |
| `IoPermissionDenied(path)`   | OS denied access                           |
| `IoAlreadyExists(path)`      | destination already exists                 |
| `IoInvalidInput(msg)`        | malformed argument                         |
| `IoInterrupted`              | syscall was interrupted                    |
| `IoUnexpectedEof`            | stream ended before the requested bytes    |
| `IoWriteZero`                | write returned 0 bytes                     |
| `IoUnknown(msg)`             | everything else                            |

`IoError` implements the built-in `Error` trait, so every value exposes
`.message() -> String` for a human-readable summary. Most call sites
destructure specific variants when they need to branch (e.g. "create a
default when the file does not exist") and fall through to `.message()`
otherwise. See [stdlib errors](errors.md) for the shared `Error` trait.

See also [bytes](bytes.md) for reading/writing binary data.


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
io.read_file(path: String) -> Result(String, IoError)
```

Reads the entire contents of a file. Returns `Ok(contents)` on success or
`Err(IoError)` on failure. When called from a spawned task, the operation
transparently yields to the scheduler while the file is being read.

```silt
import io
fn main() {
    match io.read_file("data.txt") {
        Ok(contents) -> println(contents)
        Err(IoNotFound(path)) -> println("no such file: {path}")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `io.read_line`

```
io.read_line() -> Result(String, IoError)
```

Reads a single line from stdin (trailing newline stripped). Returns
`Ok(line)` on success or `Err(IoError)` on failure — in particular
`Err(IoUnexpectedEof)` when stdin is closed. When called from a spawned
task, the operation transparently yields to the scheduler.

```silt
import io
fn main() {
    print("Name: ")
    match io.read_line() {
        Ok(name) -> println("Hello, {name}!")
        Err(IoUnexpectedEof) -> println("(EOF)")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `io.write_file`

```
io.write_file(path: String, contents: String) -> Result((), IoError)
```

Writes a string to a file, creating or overwriting it. Returns `Ok(())` on
success or `Err(IoError)` on failure. When called from a spawned task, the
operation transparently yields to the scheduler while the file is being
written.

```silt
import io
fn main() {
    match io.write_file("output.txt", "hello") {
        Ok(_) -> println("written")
        Err(e) -> println("Error: {e.message()}")
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
| `remove` | `(String) -> ()` | Unset an environment variable (idempotent) |
| `vars` | `() -> List((String, String))` | Snapshot every environment variable |


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
    println(env.get("MY_VAR"))  -- Some(hello)
}
```


## `env.remove`

```
env.remove(name: String) -> ()
```

Unsets the environment variable `name` for the current process.
Idempotent: removing a variable that is not set is not an error.

```silt
import env

fn main() {
    env.set("MY_VAR", "hello")
    env.remove("MY_VAR")
    println(env.get("MY_VAR"))  -- None

    -- Already unset — still OK.
    env.remove("MY_VAR")
}
```

Like `env.set`, `env.remove` may only be called from the main task.
Mutating the process-wide environment from a spawned task races with
any other task reading it (libc's `setenv`/`unsetenv` are not
synchronized), so the VM rejects it with an error.


## `env.vars`

```
env.vars() -> List((String, String))
```

Returns a snapshot of every environment variable as a list of
`(name, value)` pairs. The list is materialized at call time and is
not affected by subsequent `env.set` / `env.remove` calls.

```silt
import env
import list

fn main() {
    let all = env.vars()
    println("env has {list.length(all)} vars")
}
```

Ordering of the returned list is unspecified — it mirrors whatever
order the underlying platform iterator produces (typically insertion
order on glibc, but do not depend on it). If you need a stable order,
sort the result yourself. A `List` of pairs was chosen over a `Map` so
that callers who *do* want to preserve platform order can, and so
duplicate-key environments (which some shells can produce) round-trip
without silent dedup.


---

# fs

Filesystem operations: queries, directory management, and file manipulation.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `copy` | `(String, String) -> Result((), IoError)` | Copy a file |
| `exists` | `(String) -> Bool` | Check if path exists |
| `glob` | `(String) -> Result(List(String), IoError)` | Match paths by glob pattern |
| `is_dir` | `(String) -> Bool` | Check if path is a directory |
| `is_file` | `(String) -> Bool` | Check if path is a file |
| `is_symlink` | `(String) -> Bool` | Check if path is a symlink (without following) |
| `list_dir` | `(String) -> Result(List(String), IoError)` | List entries in a directory |
| `mkdir` | `(String) -> Result((), IoError)` | Create a directory (and parents) |
| `read_link` | `(String) -> Result(String, IoError)` | Read a symlink's target (without following) |
| `remove` | `(String) -> Result((), IoError)` | Remove a file or empty directory |
| `rename` | `(String, String) -> Result((), IoError)` | Rename / move a file or directory |
| `stat` | `(String) -> Result(FileStat, IoError)` | Fetch filesystem metadata for a path |
| `walk` | `(String) -> Result(List(String), IoError)` | Recursively list all paths under a directory |


## `fs.copy`

```
fs.copy(from: String, to: String) -> Result((), IoError)
```

Copies a file from `from` to `to`. Returns `Ok(())` on success or
`Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.copy("original.txt", "backup.txt") {
        Ok(_) -> println("copied")
        Err(e) -> println("Error: {e.message()}")
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
fs.list_dir(path: String) -> Result(List(String), IoError)
```

Returns `Ok(entries)` with a list of entry names in the given directory,
or `Err(IoError)` if the path does not exist or is not a directory.

```silt
import fs

import list
fn main() {
    match fs.list_dir(".") {
        Ok(entries) -> list.each(entries) { name -> println(name) }
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.mkdir`

```
fs.mkdir(path: String) -> Result((), IoError)
```

Creates a directory at `path`, including any missing parent directories.
Returns `Ok(())` on success or `Err(IoError)` on failure.

```silt
import fs

fn main() {
    match fs.mkdir("output/reports") {
        Ok(_) -> println("directory created")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.remove`

```
fs.remove(path: String) -> Result((), IoError)
```

Removes a file or an empty directory. Returns `Ok(())` on success or
`Err(message)` on failure.

```silt
import fs

fn main() {
    match fs.remove("temp.txt") {
        Ok(_) -> println("removed")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.rename`

```
fs.rename(from: String, to: String) -> Result((), IoError)
```

Renames (moves) a file or directory from `from` to `to`. Returns `Ok(())`
on success or `Err(IoError)` on failure.

```silt
import fs

fn main() {
    match fs.rename("old_name.txt", "new_name.txt") {
        Ok(_) -> println("renamed")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.stat`

```
fs.stat(path: String) -> Result(FileStat, IoError)

record FileStat {
    size: Int,                   // size in bytes
    is_file: Bool,
    is_dir: Bool,
    is_symlink: Bool,            // true if the path itself is a symlink
    modified: Int,               // modified time, unix seconds (0 if unsupported)
    readonly: Bool,              // true if the OS reports the path as read-only
    mode: Int,                   // Unix permission bits (e.g. 0o755); 0 on Windows
    accessed: Option(DateTime),  // last-access time, or None if unsupported / noatime
    created: Option(DateTime),   // creation (birth) time, or None if unsupported
}
```

Returns filesystem metadata for `path`. The stat is taken on the path
itself (via `symlink_metadata`): if the path is a symlink, `is_symlink`
is `true` and `is_file` / `is_dir` both report on the link rather than
its target. To stat the target, call `fs.read_link` and then `fs.stat`
on the result.

`mode` carries the raw Unix permission/type bits as an integer — mask
with `0o777` for permission bits, or compare against `0o040000`,
`0o100000`, etc. for file-type bits. On Windows `mode` is always `0`
because NTFS does not expose a Unix-style permission triple; use
`readonly` / `is_dir` / `is_file` instead for portable code.

`accessed` and `created` are `Option(DateTime)` because neither
timestamp is universally available:

- **`accessed`** is missing when the filesystem is mounted with
  `noatime` (common on modern Linux). Where present, it may also be
  coalesced (`relatime`) so it does not strictly reflect the *last*
  read.
- **`created`** (also called *birth time* or `btime`) is absent on
  older ext4 inodes, some network filesystems, and any platform that
  pre-dates the relevant `statx(2)` / `getattrlist(2)` surface.

Both fields are expressed as naive UTC `DateTime` records (no timezone
— see the `time` module's "naive" conventions).

Returns `Err(IoError)` when the path does not exist, permission is
denied, or the OS reports another I/O error.

```silt
import fs

fn main() {
    match fs.stat("README.md") {
        Ok(s) -> {
            println("size = {s.size}, modified = {s.modified}")
            match s.created {
                Some(dt) -> println("created at {dt.date.year}-{dt.date.month}-{dt.date.day}")
                None -> println("creation time not tracked on this filesystem")
            }
        }
        Err(e) -> println("Error: {e.message()}")
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
fs.read_link(path: String) -> Result(String, IoError)
```

Returns the raw target of a symlink (the value it points at, not the
resolved destination). Errors when `path` is not a symlink or cannot
be read.

```silt
import fs

fn main() {
    match fs.read_link("link") {
        Ok(target) -> println("points at {target}")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.walk`

```
fs.walk(root: String) -> Result(List(String), IoError)
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
returns `Err(IoUnknown("fs.walk: exceeded 1000000 entries (cap)"))` rather than
silently truncating — callers can then narrow the root or paginate at
a higher layer.

```silt
import fs

fn main() {
    match fs.walk("src") {
        Ok(paths) -> println("{paths}")
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `fs.glob`

```
fs.glob(pattern: String) -> Result(List(String), IoError)
```

Returns the list of paths matching a Unix-style glob `pattern`.
Patterns are anchored at the current working directory unless they
start with `/` (or a drive prefix on Windows). Syntax mirrors the
`glob` crate:

- `*` matches any sequence except `/`
- `?` matches a single character
- `[abc]` / `[!abc]` matches one of a character set
- `**` matches any number of directories recursively

Returns `Err(IoInvalidInput(msg))` if the pattern itself is malformed. The result
is subject to the same `1_000_000`-entry cap as `fs.walk`.

```silt
import fs

fn main() {
    match fs.glob("src/**/*.silt") {
        Ok(files) -> println("{files}")
        Err(e) -> println("Error: {e.message()}")
    }
}
```
