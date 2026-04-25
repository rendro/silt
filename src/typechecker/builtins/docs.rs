//! Shared helper for attaching markdown documentation to built-in
//! names registered in the per-module `register(checker, env)`
//! routines under `src/typechecker/builtins/`.
//!
//! Phase 2 of the LSP doc-extraction plan moved every
//! `docs/stdlib/*.md` file's prose into raw-string constants embedded
//! at the bottom of the corresponding `src/typechecker/builtins/*.rs`
//! file. The markdown is kept verbatim — the same headings, code
//! fences, and `println(...)  -- expected` annotations the doc files
//! used to carry — so the LSP can render exactly the same prose users
//! were reading on the website, and the parity walker
//! (`tests/docs_stdlib_println_parity_tests.rs`) can keep locking
//! `println` annotations against `silt run` stdout.
//!
//! The helper here parses a module's full markdown blob into per-name
//! sections by walking `##` (and `###`) headings, then attaches each
//! section body to the matching `env.bindings` entry via
//! `env.attach_doc`. A section's "name" is extracted from the heading
//! using one of two conventions found in the original `docs/stdlib`
//! files:
//!
//!   - `## \`module.name\`` → key = `module.name`
//!   - `### \`module.name\`` → key = `module.name`
//!   - `## module.name`     → key = `module.name`
//!   - `### module.name`    → key = `module.name`
//!
//! Headings whose key does not match any registered binding are
//! ignored (they're top-of-file framing like `## Summary` or `# math`).
//! `attach_module_docs` does NOT panic on unknown keys — the parity
//! walker covers that direction (every binding should have a doc)
//! while this side just attaches whatever it finds.

use crate::intern::intern;

use super::super::TypeEnv;

/// Parse a module-level markdown blob into `(heading_keys, body)`
/// pairs and attach each body to every corresponding binding in
/// `env` via `attach_doc`. Headings that don't match a binding are
/// ignored. Bodies span from the line after the heading up to (but
/// not including) the next `##` / `###` / `# ` heading, with leading
/// and trailing blank lines trimmed.
///
/// Both `## \`name\`` and `## name` heading shapes are accepted —
/// the original `docs/stdlib/*.md` files were inconsistent here.
/// Multi-name headings (`## \`time.hours\`, \`time.minutes\``) attach
/// the same body to every backticked name in the heading.
pub(super) fn attach_module_docs(env: &mut TypeEnv, md: &str) {
    for (keys, body) in iter_sections(md) {
        for key in keys {
            let sym = intern(&key);
            env.attach_doc(sym, &body);
        }
    }
}

/// Same as `attach_module_docs`, but only attaches sections whose
/// key starts with `prefix.` (or equals `prefix`). Used when one
/// markdown source mixes multiple module prefixes — `int-float.md`
/// (int + float), `io-fs.md` (io + env + fs), `result-option.md`
/// (result + option), `channel-task.md` (channel + task),
/// `encoding.md` (encoding + json), and so on. Each consumer crate
/// (`int.rs`, `float.rs`, …) calls this with its own prefix; sections
/// belonging to the other prefix are skipped (the relevant binding
/// isn't registered in this crate's scope anyway, so `attach_doc`
/// would no-op, but filtering keeps intent explicit).
#[allow(dead_code)]
pub(super) fn attach_module_docs_filtered(env: &mut TypeEnv, md: &str, prefix: &str) {
    let dot = format!("{prefix}.");
    for (keys, body) in iter_sections(md) {
        for key in keys {
            if key.starts_with(&dot) || key == prefix {
                let sym = intern(&key);
                env.attach_doc(sym, &body);
            }
        }
    }
}

/// Attach a module-level markdown blob to EVERY existing binding
/// whose name starts with `<prefix>.`. Used by modules whose
/// `docs/stdlib/*.md` source took a single-document approach (one
/// summary table + one big example, no per-function `## name`
/// sections) — `bytes`, `crypto`, `uuid`, and `stream`. Without
/// this helper, hover on `bytes.from_hex` would surface no doc at
/// all, even though the module has plenty of prose; with it,
/// hovering on any `bytes.*` name shows the whole module guide.
///
/// Per-name `## bytes.from_hex` sections (if added later) take
/// priority because callers should run `attach_module_docs` AFTER
/// `attach_module_overview` — section-specific bodies overwrite the
/// blanket overview.
/// For each `(enum_name, variants)` tuple, find the section in `md`
/// keyed `enum_name` and attach its body to every name in `variants`.
/// Used by `errors.rs` so hover on `IoNotFound` surfaces the
/// IoError section's variant table even though the section heading
/// only matches the bare enum name. Phase-2 scope decision: per-
/// variant docs are not authored separately (the table groups them
/// already), so the enum-level body is the right hover content.
#[allow(dead_code)]
pub(super) fn attach_enum_variant_docs(
    env: &mut TypeEnv,
    md: &str,
    enums: &[(&str, &[&str])],
) {
    let sections = iter_sections(md);
    for (enum_name, variants) in enums {
        let body = sections
            .iter()
            .find(|(keys, _)| keys.iter().any(|k| k == *enum_name))
            .map(|(_, body)| body.clone())
            .unwrap_or_default();
        if body.is_empty() {
            continue;
        }
        for v in *variants {
            let sym = intern(v);
            env.attach_doc(sym, &body);
        }
        // Also attach to the bare enum name itself (e.g. `IoError`
        // as a type descriptor), so hover on the type renders the
        // section. The bare enum names aren't in `env.bindings`
        // today (only their variants are), so this is a no-op for
        // current bindings — we keep the line for forward
        // compatibility if the type itself starts being registered
        // as a binding (e.g. for `type t = IoError` annotations).
        let enum_sym = intern(enum_name);
        env.attach_doc(enum_sym, &body);
    }
}

#[allow(dead_code)]
pub(super) fn attach_module_overview(env: &mut TypeEnv, md: &str, prefix: &str) {
    let dot = format!("{prefix}.");
    let names: Vec<crate::intern::Symbol> = env
        .bindings
        .keys()
        .filter(|k| crate::intern::resolve(**k).starts_with(&dot))
        .copied()
        .collect();
    for sym in names {
        env.attach_doc(sym, md);
    }
}

/// Iterate `(key, body)` pairs over a markdown string. A "section"
/// starts at a `## ` or `### ` heading whose text is either
/// `\`<key>\`` or just `<key>` (after stripping any trailing punctuation
/// like `:` or trailing whitespace). The body is everything from the
/// next line through the line before the next heading at the same or
/// lower level (or end-of-file). Leading/trailing empty lines are
/// stripped.
fn iter_sections(md: &str) -> Vec<(Vec<String>, String)> {
    let mut sections: Vec<(Vec<String>, String)> = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(keys) = parse_heading_keys(line) {
            let start = i + 1;
            let mut end = lines.len();
            for j in start..lines.len() {
                if is_section_break(lines[j]) {
                    end = j;
                    break;
                }
            }
            let body = trim_blank_edges(&lines[start..end]);
            sections.push((keys, body));
            i = end;
        } else {
            i += 1;
        }
    }
    sections
}

/// Extract heading section key(s). Returns `Some(keys)` for lines
/// shaped like:
///
///   `## \`module.name\``                        → `["module.name"]`
///   `### \`module.name\``                       → `["module.name"]`
///   `## module.name`                            → `["module.name"]`
///   `### module.name`                           → `["module.name"]`
///   `## \`time.hours\`, \`time.minutes\``       → `["time.hours", "time.minutes"]`
///
/// Other headings (`# math`, `## Summary`, etc.) return `None`.
fn parse_heading_keys(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim_start();
    let after_hash = if let Some(rest) = trimmed.strip_prefix("### ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("## ") {
        rest
    } else {
        return None;
    };
    let stripped = after_hash.trim();

    // Multi-name backticked heading: collect every backticked
    // identifier-shaped token in the heading line.
    let mut keys: Vec<String> = Vec::new();
    if stripped.starts_with('`') {
        let mut rest = stripped;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let close = match after.find('`') {
                Some(c) => c,
                None => break,
            };
            let candidate = &after[..close];
            let key = candidate
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_');
            if !key.is_empty() && looks_like_identifier(key) {
                keys.push(key.to_string());
            }
            rest = &after[close + 1..];
        }
        if keys.is_empty() {
            return None;
        }
        return Some(keys);
    }

    // Bare heading: take the first whitespace-separated token.
    let candidate = stripped.split_whitespace().next()?;
    let key = candidate.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '_');
    if key.is_empty() {
        return None;
    }
    Some(vec![key.to_string()])
}

/// Crude check that a string is identifier-shaped: at least one
/// alphabetic char, otherwise only alphanumerics, `_`, `.`. Used to
/// filter `## \`Examples:\`` (rare but conceivable) without filtering
/// `## \`time.hours\``.
fn looks_like_identifier(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic())
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Returns `true` if `line` should terminate the previous section's
/// body. Today this is any `# `, `## `, or `### ` heading. A `####`
/// heading is treated as in-section content.
fn is_section_break(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("# ")
        || trimmed.starts_with("## ")
        || trimmed.starts_with("### ")
}

/// Slice the leading and trailing blank lines off `lines` and re-join
/// with `\n`. Returns an owned string so the caller can pass it to
/// `attach_doc` (which takes `&str` but stores `String`).
fn trim_blank_edges(lines: &[&str]) -> String {
    let mut start = 0;
    while start < lines.len() && lines[start].trim().is_empty() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backticked_heading() {
        let md = "## `list.map`\nbody line\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, vec!["list.map"]);
        assert_eq!(sections[0].1, "body line");
    }

    #[test]
    fn parses_bare_heading() {
        let md = "## list.filter\nbody\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, vec!["list.filter"]);
    }

    #[test]
    fn ignores_summary_heading() {
        // `Summary` doesn't match any binding so attach is a no-op,
        // but iter_sections still returns it; that's fine.
        let md = "## Summary\nstuff\n## `list.map`\nreal body\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].0, vec!["list.map"]);
        assert_eq!(sections[1].1, "real body");
    }

    #[test]
    fn body_terminates_at_next_heading() {
        let md = "## `a.b`\none\ntwo\n## `c.d`\nthree\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].1, "one\ntwo");
        assert_eq!(sections[1].1, "three");
    }

    #[test]
    fn trims_blank_edges() {
        let md = "## `x.y`\n\n\nhi\n\n\n## `z.z`\n";
        let sections = iter_sections(md);
        assert_eq!(sections[0].1, "hi");
    }

    #[test]
    fn parses_multi_name_heading() {
        let md = "## `time.hours`, `time.minutes`, `time.seconds`\nbody\n";
        let sections = iter_sections(md);
        assert_eq!(sections.len(), 1);
        assert_eq!(
            sections[0].0,
            vec!["time.hours", "time.minutes", "time.seconds"]
        );
    }
}

// ── BEGIN AUTO-GENERATED MARKDOWN CONSTANTS ──

// Each constant below holds the verbatim contents of one of the
// former `docs/stdlib/*.md` files (round 62 phase-2 LSP doc
// inlining). Builtin-module registration sites under
// `src/typechecker/builtins/` slice these blobs by `##` heading
// via `attach_module_docs` (or `attach_module_docs_filtered`
// for split-prefix files like int-float, io-fs, result-option,
// channel-task, encoding+json) and store each section body on
// `env.builtin_docs` so the LSP can render the same prose users
// were reading on the website. Keep the markdown body verbatim:
// the parity walker (`tests/docs_stdlib_println_parity_tests.rs`)
// runs every `\u200b` (no-op) plus every `\`\`\`silt` snippet
// here against `silt run` and locks `println(x) -- expected`
// annotations against actual stdout.

/// Verbatim former `docs/stdlib/bytes.md`.
#[allow(dead_code)]
pub(super) const BYTES_MD: &str = r#"---
title: "bytes"
section: "Standard Library"
order: 16
---

# bytes

Immutable byte sequences. The `Bytes` value type carries arbitrary binary
data — useful for protocol parsing, file I/O, hashing, encoding/decoding,
and (when paired with `tcp`) network communication.

`Bytes` values use **structural equality**: two byte sequences with the
same content compare equal regardless of how they were constructed. They
work as `Map`/`Set` keys and respect the standard `==` operator.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `concat` | `(Bytes, Bytes) -> Bytes` | Concatenate two byte sequences |
| `concat_all` | `(List(Bytes)) -> Bytes` | Concatenate every element of a list |
| `empty` | `() -> Bytes` | Zero-length byte sequence |
| `ends_with` | `(Bytes, Bytes) -> Bool` | True if `b` ends with `suffix` |
| `eq` | `(Bytes, Bytes) -> Bool` | Structural byte-by-byte comparison |
| `from_base64` | `(String) -> Result(Bytes, BytesError)` | Decode base64 string |
| `from_hex` | `(String) -> Result(Bytes, BytesError)` | Decode hex string (case-insensitive) |
| `from_list` | `(List(Int)) -> Result(Bytes, BytesError)` | Build from a list of byte values (0..=255) |
| `from_string` | `(String) -> Bytes` | UTF-8 encode a string |
| `get` | `(Bytes, Int) -> Result(Int, BytesError)` | Read a single byte at index |
| `index_of` | `(Bytes, Bytes) -> Option(Int)` | First offset at which `needle` appears |
| `length` | `(Bytes) -> Int` | Number of bytes |
| `slice` | `(Bytes, Int, Int) -> Result(Bytes, BytesError)` | Half-open `[start, end)` slice |
| `split` | `(Bytes, Bytes) -> List(Bytes)` | Split on every occurrence of `sep` (panics if `sep` is empty) |
| `starts_with` | `(Bytes, Bytes) -> Bool` | True if `b` begins with `prefix` |
| `to_base64` | `(Bytes) -> String` | Encode as base64 |
| `to_hex` | `(Bytes) -> String` | Encode as lowercase hex |
| `to_list` | `(Bytes) -> List(Int)` | Materialize as a list of byte values |
| `to_string` | `(Bytes) -> Result(String, BytesError)` | UTF-8 decode (errors on invalid UTF-8) |

## Examples

```silt
import bytes

fn main() {
  -- Construction
  let hello = bytes.from_string("hello")           -- 5 bytes
  let raw = match bytes.from_hex("deadbeef") {     -- 4 bytes
    Ok(b) -> b
    Err(_) -> bytes.empty()
  }

  -- Length and access
  println(bytes.length(hello))                     -- 5
  match bytes.get(hello, 0) {
    Ok(n) -> println(n)                            -- 104
    Err(e) -> println(e.message())
  }

  -- Encoding
  println(bytes.to_hex(hello))                     -- 68656c6c6f
  println(bytes.to_base64(hello))                  -- aGVsbG8=

  -- Concatenation
  let space = bytes.from_string(" ")
  let world = bytes.from_string("world")
  let greeting = bytes.concat_all([hello, space, world])
  match bytes.to_string(greeting) {
    Ok(s) -> println(s)                            -- hello world
    Err(e) -> println(e.message())
  }

  -- Slicing (half-open)
  match bytes.slice(greeting, 6, 11) {
    Ok(s) -> println(bytes.to_hex(s))              -- 776f726c64
    Err(e) -> println(e.message())
  }

  -- Equality is structural
  let a = bytes.from_string("foo")
  let b = bytes.from_string("foo")
  println(a == b)                                  -- true

  -- Search / prefix / suffix / split
  let msg = bytes.from_string("foo::bar::baz")
  let sep = bytes.from_string("::")
  match bytes.index_of(msg, sep) {
    Some(i) -> println(i)                          -- 3
    None -> println(-1)
  }
  println(bytes.starts_with(msg, bytes.from_string("foo")))  -- true
  println(bytes.ends_with(msg, bytes.from_string("baz")))    -- true
  -- bytes.split yields [foo, bar, baz] as three Bytes values.
  let parts = bytes.split(msg, sep)
}
```

## Errors

Every fallible `bytes.*` call returns `Result(T, BytesError)`. The enum
exposes five variants keyed by the structural failure; pattern-match
for granular handling or fall back to `e.message()`:

| Variant | Fields | Raised by |
|---------|--------|-----------|
| `BytesInvalidUtf8(offset)` | `Int` | `to_string` on non-UTF-8 input |
| `BytesInvalidHex(msg)` | `String` | `from_hex` on odd length / non-hex char |
| `BytesInvalidBase64(msg)` | `String` | `from_base64` on malformed input |
| `BytesByteOutOfRange(value)` | `Int` | `from_list` when an element is negative or `> 255` |
| `BytesOutOfBounds(index)` | `Int` | `slice` / `get` on an out-of-range index |

`BytesError` implements the built-in `Error` trait.

## Notes

- `Bytes` is allocated once and shared by `Arc` internally — passing the
  same `Bytes` through many functions does not copy the underlying buffer.
- Display format is `bytes(<hex preview, up to 32 bytes>, length: N)`,
  intended for debugging output. Use `bytes.to_hex` or `bytes.to_base64`
  for stable serialization.
- `bytes.index_of` returns `Some(0)` for an empty `needle`. `bytes.starts_with`
  and `bytes.ends_with` return `true` for an empty prefix / suffix.
- `bytes.split` panics if `sep` is empty (ambiguous). Splitting an empty
  `b` yields `[empty_bytes]` — one element — mirroring `string.split`.
- A future silt release may promote `Bytes` to a language-level type with
  literal syntax (e.g. `b"hello"`) and method-form access. Today's API
  is forward-compatible: programs written against the current `bytes`
  module surface will continue to behave identically.
"#;

/// Verbatim former `docs/stdlib/channel-task.md`.
#[allow(dead_code)]
pub(super) const CHANNEL_TASK_MD: &str = r#"---
title: "channel / task"
section: "Standard Library"
order: 12
---

# channel

Bounded channels for concurrent task communication. Channels provide
communication between tasks spawned with `task.spawn`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `close` | `(Channel) -> ()` | Close the channel |
| `each` | `(Channel, (a) -> b) -> ()` | Iterate until channel closes |
| `new` | `(Int?) -> Channel` | Create a channel (0 = rendezvous, N = buffered) |
| `receive` | `(Channel) -> ChannelResult(a)` | Blocking receive |
| `recv_timeout` | `(Channel(a), Duration) -> Result(a, ChannelError)` | Blocking receive with a timeout |
| `select` | `(List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))` | Wait on multiple channels (each op is `Recv(ch)` or `Send(ch, v)`) |
| `send` | `(Channel, a) -> ()` | Blocking send |
| `timeout` | `(Int) -> Channel` | Create a channel that closes after N ms |
| `try_receive` | `(Channel) -> ChannelResult(a)` | Non-blocking receive |
| `try_send` | `(Channel, a) -> Bool` | Non-blocking send |


## `channel.close`

```
channel.close(ch: Channel) -> ()
```

Closes the channel. Subsequent sends will fail. Receivers will see `Closed`
after all buffered messages are consumed.

```silt
import channel
fn main() {
    let ch = channel.new(10)
    channel.send(ch, 1)
    channel.close(ch)
}
```


## `channel.each`

```
channel.each(ch: Channel, f: (a) -> b) -> ()
```

Receives messages from the channel and calls `f` with each one, until the
channel is closed. This is the idiomatic way to consume all messages.

```silt
import channel
import task
fn main() {
    let ch = channel.new(10)
    task.spawn(fn() {
        channel.send(ch, 1)
        channel.send(ch, 2)
        channel.close(ch)
    })
    channel.each(ch) { msg -> println(msg) }
    -- prints 1, then 2
}
```


## `channel.new`

```
channel.new() -> Channel
channel.new(capacity: Int) -> Channel
```

Creates a new channel. With no argument, creates a rendezvous channel
(capacity 0) where the sender blocks until a receiver is ready and vice versa.
With an integer argument, creates a buffered channel with that capacity --
sends block when the buffer is full, receives block when the buffer is empty.

```silt
import channel
fn main() {
    let rendezvous = channel.new()    -- true rendezvous (capacity 0)
    let buffered = channel.new(10)    -- buffered (capacity 10)
}
```


## `channel.receive`

```
channel.receive(ch: Channel) -> ChannelResult(a)
```

Receives a value from the channel. Returns `Message(value)` when a value is
available, or `Closed` when the channel is closed and empty. Parks the task
while waiting, allowing other tasks to run on the same thread.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    match channel.receive(ch) {
        Message(v) -> println(v)
        Closed -> println("done")
        _ -> ()
    }
}
```


## `channel.recv_timeout`

```
channel.recv_timeout(ch: Channel(a), dur: Duration) -> Result(a, ChannelError)
```

Blocking receive with a scoped timeout. Returns:

- `Ok(value)` if a value is delivered within `dur`.
- `Err(ChannelTimeout)` if `dur` elapses with no value and no close.
- `Err(ChannelClosed)` if the channel is closed and has no more buffered values.

A value already buffered, or a rendezvous sender already parked, wins over an
expired timer: the non-blocking path is always tried first so readiness is not
preempted by the timer. A `Duration` of zero gives try-receive semantics (never
schedules a timer); negative durations are a construction error. Positive
sub-millisecond durations are rounded up to one millisecond so the caller
always gets at least one timer tick of wait.

This uses the shared timer thread that backs `channel.timeout` and `time.sleep`
-- no per-call OS thread. Cancelling the surrounding `task.spawn` handle
cleans up both the channel-side waker registration and the timer registration.

`ChannelError` implements the built-in `Error` trait, so `e.message()`
renders either variant as a string:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ChannelTimeout` | — | timer elapsed before a value arrived |
| `ChannelClosed` | — | channel closed with no more values |

```silt
import channel
import task
import time

fn main() {
    let ch = channel.new(0)
    task.spawn(fn() {
        time.sleep(time.ms(50))
        channel.send(ch, 42)
    })
    match channel.recv_timeout(ch, time.ms(500)) {
        Ok(v) -> println(v)                   -- 42
        Err(ChannelTimeout) -> println("timed out")
        Err(ChannelClosed) -> println("channel closed")
    }
}
```


## `channel.select`

```
channel.select(ops: List(ChannelOp(a))) -> (Channel(a), ChannelResult(a))
```

Waits until one of the operations in `ops` can make progress. Every element
is a `ChannelOp(a)` value built with one of two constructors:

- `Recv(ch)` — a **receive** arm that becomes ready when `ch` has a buffered
  value, has a rendezvous sender parked on it, or is closed;
- `Send(ch, value)` — a **send** arm that becomes ready when `ch` has buffer
  capacity or a rendezvous receiver parked on it.

The call returns a 2-tuple of `(channel, result)` identifying the arm that
won and the outcome:

- `(ch, Message(val))` — a `Recv` arm completed with `val`.
- `(ch, Closed)` — a `Recv` arm's channel is closed and drained.
- `(ch, Sent)` — a `Send` arm completed (the value was handed off).

Receive and send arms can be mixed freely in the same call.

Receive-only form:

```silt
import channel
import task
fn main() {
    let ch1 = channel.new(1)
    let ch2 = channel.new(1)
    task.spawn(fn() { channel.send(ch2, "hello") })
    match channel.select([Recv(ch1), Recv(ch2)]) {
        (^ch2, Message(val)) -> println(val)  -- hello
        (_, Closed) -> println("closed")
        _ -> ()
    }
}
```

Mixed send and receive — race a pending send against an incoming receive:

```silt
import channel
fn main() {
    let inbox = channel.new(1)
    let outbox = channel.new(1)
    channel.send(inbox, 7)
    match channel.select([Recv(inbox), Send(outbox, 99)]) {
        (^inbox, Message(v)) -> println(v)
        (^outbox, Sent) -> println("sent")
        _ -> ()
    }
}
```


## `channel.send`

```
channel.send(ch: Channel, value: a) -> ()
```

Sends a value into the channel. Parks the task if the buffer is full, allowing
other tasks to run until space opens up.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, "hello")
}
```


## `channel.timeout`

```
channel.timeout(ms: Int) -> Channel
```

Creates a channel that automatically closes after the given number of
milliseconds. The returned channel carries no values -- it simply closes when
the duration elapses. This is useful for adding deadlines to `channel.select`.

```silt
import channel
fn main() {
    let ch = channel.new(10)
    let timer = channel.timeout(1000)  -- closes after 1 second
    match channel.select([Recv(ch), Recv(timer)]) {
        (^ch, Message(val)) -> println("got: {val}")
        (^timer, Closed) -> println("timed out")
        _ -> ()
    }
}
```


## `channel.try_receive`

```
channel.try_receive(ch: Channel) -> ChannelResult(a)
```

Non-blocking receive. Returns `Message(value)` if a value is immediately
available, `Empty` if the channel is open but has no data, or `Closed` if the
channel is closed and empty.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    match channel.try_receive(ch) {
        Message(v) -> println(v)
        Empty -> println("nothing yet")
        Closed -> println("done")
        _ -> ()
    }
}
```


## `channel.try_send`

```
channel.try_send(ch: Channel, value: a) -> Bool
```

Non-blocking send. Returns `true` if the value was successfully buffered,
`false` if the buffer is full or the channel is closed.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    let ok = channel.try_send(ch, 42)
    println(ok)  -- true
}
```


---

# task

Spawn and coordinate lightweight concurrent tasks. Tasks are multiplexed onto a
fixed thread pool and run in parallel. They communicate through channels.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `cancel` | `(Handle) -> ()` | Request cancellation of a task (cooperative; see details below) |
| `deadline` | `(Duration, () -> a) -> a` | Run a callback with a scoped I/O deadline |
| `join` | `(Handle) -> a` | Wait for a task to complete |
| `spawn` | `(() -> a) -> Handle` | Spawn a new lightweight task |
| `spawn_until` | `(Duration, () -> a) -> Handle(a)` | Spawn a task scoped by a deadline |


## `task.cancel`

```
task.cancel(handle: Handle) -> ()
```

Flips the handle's result slot to `Err("cancelled")` using first-writer-wins
semantics: if the task has already completed with some other result,
`task.cancel` is a no-op on the handle. This is **not** a synchronous stop
signal — treat it as a cooperative request, not a hard stop:

- If the task is **currently parked** (blocked on a channel, `task.join`,
  `time.sleep`, or a timer), the pending wake registrations are torn down and
  the task will not be resumed. The handle resolves to `Err("cancelled")`.
- If the task is **currently running**, the handle's result is set
  immediately, but the running slice continues executing until its next
  cooperative yield point or natural completion. Any side effects the slice
  performs before it next parks — writes, spawns, channel sends, I/O — run to
  completion. Its own final result is then discarded (first-writer-wins).

`task.join` on a cancelled handle does **not** return `Err("cancelled")`
as a value — it raises the failure as a runtime error of the form
`joined task failed: cancelled`. Silt has no `try`/`catch`, so when
cancellation is an expected outcome you typically either (a) signal
completion through a sentinel channel and wait on that instead of joining,
or (b) scope the join to a boundary where the raised error is the expected
exit path. See
[Concurrency: Cancelling](../concurrency.md#cancelling-taskcancelhandle) for
the canonical treatment.

```silt
-- noexec
import channel
import task
fn main() {
    let done = channel.new(1)
    let h = task.spawn(fn() {
        -- long-running work
        channel.send(done, 42)
    })
    task.cancel(h)
    -- `task.join(h)` here would raise `joined task failed: cancelled`.
    -- Use the sentinel channel for a non-raising "settled" signal, or
    -- only call `task.join` at a boundary that tolerates the raise.
}
```


## `task.join`

```
task.join(handle: Handle) -> a  -- raises on failure
```

Blocks until the task completes and returns its result. Parks the calling task
while waiting, allowing other tasks to run.

If the joined task panicked, errored, or was cancelled, `task.join` does
**not** surface the failure as an `Err` value. It raises a runtime error of
the form `joined task failed: <msg>` at the call site (e.g. `joined task
failed: cancelled` for a cancelled handle, `joined task failed: division by
zero` for a panicking task body). Silt has no `try`/`catch`, so a joined
failure is terminal for the joining task — when cancellation or task
failure is an expected outcome, use a channel handshake or sentinel value
instead of relying on `task.join` for the signal.

```silt
import task
fn main() {
    let h = task.spawn(fn() { 1 + 2 })
    let sum = task.join(h)
    println(sum)  -- 3
}
```


## `task.spawn`

```
task.spawn(f: () -> a) -> Handle
```

Spawns a zero-argument function as a lightweight task on the thread pool.
Spawning is cheap -- it allocates a stack, not an OS thread. Returns a handle
that can be used with `task.join` or `task.cancel`.

```silt
import task
fn main() {
    let h = task.spawn(fn() {
        println("running in a task")
        42
    })
    let answer = task.join(h)
    println(answer)  -- 42
}
```


## `task.deadline`

```
task.deadline(dur: Duration, f: () -> a) -> a
```

Runs `f` with a scoped I/O deadline. If any blocking I/O builtin inside `f`
(see [Concurrency: Blocking operations](../concurrency.md#blocking-operations))
runs longer than `dur`, the builtin returns **the module's own typed
timeout variant** instead of its normal result — the surrounding silt code
handles it through the usual `Result` match on the typed `IoError`,
`TcpError`, or `HttpError` enum that builtin already declares. No exception
is raised, and the deadline does not preempt pure CPU work; it only applies
to I/O.

Specifically (matching the [`SILT_IO_TIMEOUT`](../concurrency.md#io-timeouts-silt_io_timeout)
table):

- `io.*` and `fs.*` surface `Err(IoUnknown("I/O timeout (task.deadline exceeded)"))`.
- `tcp.*` surfaces `Err(TcpTimeout)`.
- `http.*` surfaces `Err(HttpTimeout)`.

The deadline is *scoped*: it nests cleanly with an outer `SILT_IO_TIMEOUT`
or a surrounding `task.deadline`, whichever elapses first fires. The
embedded message on the `IoUnknown` variant distinguishes the source so
silt code can tell scoped timeouts from the global one.

```silt
-- noexec
import io
import task
import time

fn main() {
    let outcome = task.deadline(time.ms(200), fn() {
        io.read_file("/var/log/slow.log")
    })
    match outcome {
        Ok(contents) -> println(contents)
        Err(IoUnknown(msg)) -> println(msg)  -- I/O timeout (task.deadline exceeded)
        Err(_) -> println("other io error")
    }
}
```


## `task.spawn_until`

```
task.spawn_until(dur: Duration, f: () -> a) -> Handle(a)
```

Spawns `f` as a task with a bounded wall-clock deadline. Equivalent to
`task.spawn(fn() { task.deadline(dur, f) })` but with one less closure
wrapper. The returned handle resolves to the function's result if it
finishes in time, or to the deadline error inside any I/O builtin it
was blocked on when the deadline fired.

Useful for fan-out patterns where each child task must bound its own
runtime -- e.g. racing N replicas and dropping stragglers.

```silt
-- noexec
import io
import task
import time

fn main() {
    let h = task.spawn_until(time.seconds(2), fn() {
        io.read_file("/tmp/maybe_slow.txt")
    })
    match task.join(h) {
        Ok(contents) -> println(contents)
        Err(msg) -> println(msg)
    }
}
```
"#;

/// Verbatim former `docs/stdlib/crypto.md`.
#[allow(dead_code)]
pub(super) const CRYPTO_MD: &str = r#"---
title: "crypto"
section: "Standard Library"
order: 17
---

# crypto

Cryptographic primitives: SHA-256 / SHA-512 / BLAKE2b / MD5 hashing,
HMAC message authentication, an OS-backed CSPRNG, and a timing-safe
byte-comparison routine. Most functions consume and produce `Bytes`
values — pipe through `bytes.to_hex` / `bytes.to_base64` when you need
a string representation, or use the `_hex` convenience variants (e.g.
`md5_hex`, `blake2b_hex`).

`crypto.random_bytes` reads directly from the operating system's
cryptographically secure pseudo-random number generator (`getrandom(2)`
on Linux, `BCryptGenRandom` on Windows, `SecRandomCopyBytes` on macOS;
see the [getrandom](https://crates.io/crates/getrandom) crate for the
full platform list). The `crypto.constant_time_eq` comparison is
timing-safe for equal-length inputs: the running time is independent of
where a mismatch occurs. Length mismatches short-circuit to `false`,
matching the convention used by Python's `hmac.compare_digest` and
Rust's `subtle` crate — this means lengths can leak via timing but the
contents of equal-length buffers cannot. Pad inputs to a fixed width
beforehand if length privacy matters for your protocol.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `sha256` | `(Bytes) -> Bytes` | SHA-256 digest (32 bytes) |
| `sha512` | `(Bytes) -> Bytes` | SHA-512 digest (64 bytes) |
| `md5` | `(Bytes) -> Bytes` | MD5 digest (16 bytes) — **legacy / non-security use only** |
| `md5_hex` | `(Bytes) -> String` | MD5 digest as lower-case hex (32 chars) |
| `blake2b` | `(Bytes) -> Bytes` | BLAKE2b-512 digest (64 bytes), RFC 7693 |
| `blake2b_hex` | `(Bytes) -> String` | BLAKE2b-512 digest as lower-case hex (128 chars) |
| `hmac_sha256` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA256 over `(key, msg)` (32 bytes) |
| `hmac_sha512` | `(Bytes, Bytes) -> Bytes` | HMAC-SHA512 over `(key, msg)` (64 bytes) |
| `random_bytes` | `(Int) -> Result(Bytes, String)` | OS CSPRNG, `0..=1_048_576` bytes |
| `constant_time_eq` | `(Bytes, Bytes) -> Bool` | Timing-safe comparison (lengths leak) |

## Examples

```silt
import bytes
import crypto

fn main() {
  -- Hashing
  let msg = bytes.from_string("abc")
  let digest = crypto.sha256(msg)
  println(bytes.to_hex(digest))
  -- ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad

  -- HMAC authentication
  let key = bytes.from_string("secret key")
  let tag = crypto.hmac_sha256(key, bytes.from_string("hello"))
  println(bytes.length(tag))                       -- 32

  -- CSPRNG: 32 random bytes (256-bit token)
  match crypto.random_bytes(32) {
    Ok(token) -> println(bytes.to_hex(token))
    Err(e) -> println(e)
  }

  -- Timing-safe comparison for auth tag verification
  let expected = crypto.hmac_sha256(key, bytes.from_string("hello"))
  let received = crypto.hmac_sha256(key, bytes.from_string("hello"))
  println(crypto.constant_time_eq(expected, received))  -- true
}
```

## Errors

Only `random_bytes` is fallible at the type level — the others are
total functions over their `Bytes` inputs.

| Operation | Error condition |
|-----------|-----------------|
| `random_bytes` | `n < 0` ("n must be non-negative"); `n > 1_048_576` ("n exceeds 1 MiB cap"); OS CSPRNG failure |

## Notes

- Digest outputs have fixed widths: `16` bytes (`md5`), `32` bytes
  (`sha256`, `hmac_sha256`), `64` bytes (`sha512`, `hmac_sha512`,
  `blake2b`). The typechecker has no dependent-type support for
  fixed-width `Bytes`, so the returned type is the same opaque `Bytes`
  the rest of the module uses.
- **MD5 is not collision-resistant.** Use it only for interop with
  legacy systems (etags, Git-style content hashing, cache keys where
  an adversary isn't in play). Never use MD5 for signatures,
  certificates, or any security decision — prefer `sha256` or
  `blake2b`.
- `blake2b` is BLAKE2b-512 (the 512-bit-output variant from RFC 7693).
  It's faster than SHA-512 on 64-bit hardware and is a sound default
  for new protocols that don't need to match a SHA-family spec.
- The `_hex` variants (`md5_hex`, `blake2b_hex`) exist so common use
  cases (log lines, cache keys) don't need a round-trip through
  `bytes.to_hex`. Output is always lower-case hex.
- `random_bytes(0)` returns an empty `Bytes` (not an error). This keeps
  caller code simpler when the length comes from a variable.
- The 1 MiB cap on `random_bytes` is a sanity guard against accidental
  huge allocations; it is not a security boundary. Loop if you really
  need more than a mebibyte of entropy at a time.
- Use `crypto.constant_time_eq` — not `bytes.eq` and not the `==`
  operator — whenever you compare a user-supplied value against a
  secret (MAC tag, password hash, token). The `bytes.eq` path
  short-circuits on the first differing byte and leaks the matching
  prefix length via timing.
"#;

/// Verbatim former `docs/stdlib/encoding.md`.
#[allow(dead_code)]
pub(super) const ENCODING_MD: &str = r#"---
title: "encoding"
section: "Standard Library"
order: 18
---

# encoding

URL / percent encoding per [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986).
This module is intentionally narrow: base64 and hex encoding live on
the [`bytes`](bytes.md) module (`bytes.to_base64` / `bytes.from_base64` /
`bytes.to_hex` / `bytes.from_hex`) because they operate on `Bytes`, not
`String`. Percent-encoding, by contrast, is a `String` ↔ `String`
transform — the input is text destined for a URL (query-string value,
path segment, fragment) and the output is text.

RFC 3986 §2.3 defines the **unreserved** set that never needs encoding:

```
ALPHA / DIGIT / "-" / "." / "_" / "~"
```

Every other byte of the UTF-8 representation is emitted as `%HH` with
upper-case hex digits (per §6.2.2.1's case normalization note). Decoding
is case-insensitive: `%2F` and `%2f` both decode to `/`.

`+` is a **literal `+`** in both directions. The `+ ↔ space` convention
belongs to `application/x-www-form-urlencoded` (WHATWG URL §form-urlencoded),
not RFC 3986, and is out of scope here. Build form-encoding on top of
`encoding.url_encode` in a dedicated module if you need it.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `url_encode` | `(String) -> String` | Percent-encode per RFC 3986 (unreserved = `ALPHA` / `DIGIT` / `-._~`) |
| `url_decode` | `(String) -> Result(String, String)` | Inverse. Errors on malformed `%HH` or invalid UTF-8 after decoding |
| `form_encode` | `(List((String, String))) -> String` | Build an `application/x-www-form-urlencoded` body |
| `form_decode` | `(String) -> Result(List((String, String)), String)` | Parse an `application/x-www-form-urlencoded` body into pairs |

## Examples

```silt
import encoding

fn main() {
  -- Safely embed user-supplied text in a query-string value.
  let query = "hello world & goodbye?"
  let encoded = encoding.url_encode(query)
  println(encoded)
  -- hello%20world%20%26%20goodbye%3F

  -- Round-trip.
  match encoding.url_decode(encoded) {
    Ok(back) -> println(back)              -- hello world & goodbye?
    Err(e) -> println(e)
  }

  -- Non-ASCII: UTF-8 bytes are encoded.
  println(encoding.url_encode("café"))     -- caf%C3%A9

  -- Malformed input is rejected.
  match encoding.url_decode("bad%") {
    Ok(_) -> println("should not happen")
    Err(e) -> println(e)                   -- truncated percent-escape at offset 3
  }
}
```

## Errors

Only `url_decode` is fallible — `url_encode` is a total function over
any `String`.

| Operation | Error condition |
|-----------|-----------------|
| `url_decode` | Truncated `%` at end of string (e.g. `"bad%"`) |
| `url_decode` | Non-hex digits after `%` (e.g. `"bad%ZZ"`) |
| `url_decode` | Decoded byte sequence is not valid UTF-8 (e.g. `"%C3%28"`) |

## Notes

- The encoder always emits upper-case hex (`%2F`, not `%2f`). The
  decoder accepts both cases. This matches the RFC's case-normalization
  recommendation (§6.2.2.1) for producers.
- `url_encode` is *not* a query-string builder. For `key=value&key=value`
  assembly, `url_encode` each key and each value separately and join
  the resulting strings yourself. That separation keeps the primitive
  honest — you can use it for path segments, fragments, and header
  values too, not just query parameters.
- Binary payloads should go through `bytes.to_base64` (or `bytes.to_hex`)
  first, then the resulting ASCII string can be fed to `url_encode` if
  it still needs URL-safety on top of base64.

## `form_encode`

```
encoding.form_encode(pairs: List((String, String))) -> String
```

Produces an `application/x-www-form-urlencoded` body. Each `(key, value)`
pair becomes `key=value`; both halves are percent-escaped with the
WHATWG form-urlencoded byte set (space → `+`, `*-._` plus
alphanumerics pass through, everything else becomes `%HH` with
upper-case hex); pairs are joined with `&`. Input order is preserved
in the output, so callers can build deterministic signatures. An empty
list produces the empty string.

```silt
import encoding

fn main() {
  let body = encoding.form_encode([
    ("name", "Ada Lovelace"),
    ("role", "analyst & author"),
    ("lang", "English"),
  ])
  println(body)
  -- name=Ada+Lovelace&role=analyst+%26+author&lang=English
}
```

The signature takes `List((String, String))` rather than
`Map(String, String)` on purpose: order matters for APIs that sign or
hash the encoded body (OAuth 1.0a, S3 canonical query strings, etc.),
and a `List` preserves it. It also lets callers represent duplicate
keys, which are legal in form bodies.

## `form_decode`

```
encoding.form_decode(body: String) -> Result(List((String, String)), String)
```

Inverse of `form_encode`. Splits the body on `&`, splits each segment
on its first `=`, and decodes both halves: `+` becomes a space, `%HH`
becomes the corresponding byte, and the combined byte sequence must be
valid UTF-8. A segment with no `=` is treated as `(key, "")`. Empty
segments (produced by leading, trailing, or doubled `&`) are silently
skipped, matching the WHATWG URL parser. Order is preserved.

```silt
import encoding

fn main() {
  match encoding.form_decode("a=1&b=hello+world&c=%26") {
    Ok(pairs) -> println(pairs)
    -- [("a", "1"), ("b", "hello world"), ("c", "&")]
    Err(e) -> println(e)
  }
}
```

Malformed percent escapes or invalid UTF-8 surface as `Err(msg)` with
a message identifying which pair and half (key / value) was bad.
"#;

/// Verbatim former `docs/stdlib/errors.md`.
#[allow(dead_code)]
pub(super) const ERRORS_MD: &str = r#"---
title: "stdlib errors"
section: "Standard Library"
order: 20
---

# Stdlib typed errors

Every fallible stdlib module declares its own typed error enum. Silt's
`Result(T, ModuleError)` return shape lets user code pattern-match the
failure modes directly instead of substring-matching on a `String`
payload. Each enum implements the built-in `Error` trait, which
supertypes `Display` and provides a `message()` method so code that
just wants a rendered error can fall back to `"{e.message()}"`.

## Variant naming

Every variant is module-prefixed (`IoNotFound`, not `NotFound`) so
silt's one-variant-per-enum registration never collides. Each variant
is globally unique and may be constructed either bare or with its enum
as qualifier:

```silt
import io

let a = IoNotFound("config.toml")
let b = IoError.IoNotFound("config.toml")  -- same value
```

Construction is gated on the owning module being imported — bare
`IoNotFound(...)` without `import io` is a compile error. Pattern
matching is not gated: once you hold a value, you can destructure it
regardless of imports.

## Enums

### `IoError` (requires `import io`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `IoNotFound(String)` | path | file or directory missing |
| `IoPermissionDenied(String)` | path | permissions check failed |
| `IoAlreadyExists(String)` | path | target already exists |
| `IoInvalidInput(String)` | description | malformed argument |
| `IoInterrupted` | — | syscall interrupted |
| `IoUnexpectedEof` | — | reader hit EOF mid-record |
| `IoWriteZero` | — | writer returned zero bytes |
| `IoUnknown(String)` | platform message | unclassified platform error |

### `JsonError` (requires `import json`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `JsonSyntax(String, Int)` | message, byte offset | syntactically invalid JSON |
| `JsonTypeMismatch(String, String)` | expected, actual | wrong JSON type for target |
| `JsonMissingField(String)` | field name | required field absent |
| `JsonUnknown(String)` | message | unclassified parse failure |

### `TomlError` (requires `import toml`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TomlSyntax(String, Int)` | message, byte offset | syntactically invalid TOML |
| `TomlTypeMismatch(String, String)` | expected, actual | wrong TOML type for target |
| `TomlMissingField(String)` | field name | required field absent |
| `TomlUnknown(String)` | message | unclassified parse failure |

### `ParseError` (requires `import int` or `import float`)

Shared by `int.parse` and `float.parse`. Either import unlocks the
variants; users who import only one can still match on values they
receive from the other.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ParseEmpty` | — | input was empty |
| `ParseInvalidDigit(Int)` | byte offset | non-digit character at offset |
| `ParseOverflow` | — | value exceeds type max |
| `ParseUnderflow` | — | value below type min |

### `HttpError` (requires `import http`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `HttpConnect(String)` | message | TCP / DNS connect failure |
| `HttpTls(String)` | message | TLS handshake / cert failure |
| `HttpTimeout` | — | request exceeded its deadline |
| `HttpInvalidUrl(String)` | url | URL did not parse |
| `HttpInvalidResponse(String)` | message | response violated protocol |
| `HttpClosedEarly` | — | peer closed before response completed |
| `HttpStatusCode(Int, String)` | status, body preview | non-success status |
| `HttpUnknown(String)` | message | unclassified transport error |

### `TcpError` (requires `import tcp`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TcpConnect(String)` | message | tcp/dns connect failure |
| `TcpTls(String)` | message | TLS handshake failure |
| `TcpClosed` | — | connection closed (broken pipe, peer reset) |
| `TcpTimeout` | — | op exceeded its deadline |
| `TcpUnknown(String)` | message | unclassified socket failure |

### `PgError` (requires `import postgres`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `PgConnect(String)` | message | tcp / DNS / pool checkout failure |
| `PgTls(String)` | message | TLS setup / handshake |
| `PgAuthFailed(String)` | message | SQLSTATE class 28 |
| `PgQuery(String, String)` | message, SQLSTATE | server-reported error |
| `PgTypeMismatch(String, String, String)` | column, expected, actual | row decode |
| `PgNoSuchColumn(String)` | column | row.get on missing column |
| `PgClosed` | — | connection dropped mid-query |
| `PgTimeout` | — | statement / pool timeout |
| `PgTxnAborted` | — | SQLSTATE 25P02 — rollback required |
| `PgUnknown(String)` | message | unclassified pg error |

### `TimeError` (requires `import time`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TimeParseFormat(String)` | message | pattern did not match input |
| `TimeOutOfRange(String)` | message | field out of valid range (e.g. month=13) |

### `BytesError` (requires `import bytes`)

| Variant | Fields | Meaning |
|---------|--------|---------|
| `BytesInvalidUtf8(Int)` | byte offset | decode failed at offset |
| `BytesInvalidHex(String)` | message | bad hex string |
| `BytesInvalidBase64(String)` | message | bad base64 string |
| `BytesByteOutOfRange(Int)` | value | list element outside 0..=255 |
| `BytesOutOfBounds(Int)` | index | slice or get index out of bounds |

### `ChannelError` (requires `import channel`)

Returned by `channel.recv_timeout`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `ChannelTimeout` | — | timer elapsed before a value arrived |
| `ChannelClosed` | — | channel closed with no more values |

### `RegexError` (requires `import regex`)

Constructible by user code. Stdlib `regex.*` functions do not return
`Result(_, RegexError)` — their signatures return `Bool` / `Option` /
`List` / `String`, and an invalid pattern surfaces as a runtime error
at the call site rather than through `Err`.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `RegexInvalidPattern(String, Int)` | message, byte offset | pattern did not parse |
| `RegexTooBig` | — | compiled pattern exceeded size budget |

## Stdlib functions that return `Result(T, String)`

A handful of fallible stdlib functions surface their error as a plain
`String` rather than a typed enum — the failure modes are not diverse
enough to benefit from a richer taxonomy:

- `encoding.url_decode`, `encoding.form_decode`
- `crypto.random_bytes`
- `uuid.parse`

## Example: user-side pattern matching

```silt
import io

fn handle(e: IoError) -> String {
  match e {
    IoNotFound(path) -> "missing: {path}"
    IoPermissionDenied(path) -> "denied: {path}"
    IoAlreadyExists(_) | IoInvalidInput(_) -> "recoverable"
    IoInterrupted | IoUnexpectedEof | IoWriteZero -> "transient"
    IoUnknown(msg) -> "unknown: {msg}"
  }
}

fn main() {
  println(handle(IoNotFound("config.toml")))
}
```

## Cross-module composition

Silt does not auto-convert between error types. A function that spans
several stdlib modules wraps each module's error in a local `AppError`
enum and lifts each call's `Err` into it with `result.map_err`. Variant
constructors are first-class `Fn(e) -> f` values, so the second argument
is just the constructor name — no closure wrapper needed:

```silt
import io
import json
import result

type Config { host: String, port: Int }

type AppError {
  IoProblem(IoError),
  JsonProblem(JsonError),
}

fn load_config(path: String) -> Result(Config, AppError) {
  let raw = io.read_file(path) |> result.map_err(IoProblem)?
  let cfg = json.parse(raw, Config) |> result.map_err(JsonProblem)?
  Ok(cfg)
}
```

`?` binds looser than `|>`, so the whole pipeline is a single expression
terminated by `?`. See [`examples/cross_module_errors.silt`](../../examples/cross_module_errors.silt)
for a longer walkthrough. A separate proposal
([`error-from-trait.md`](../proposals/error-from-trait.md)) tracks the
design for a `.into()`-based ergonomics layer over this pattern.
"#;

/// Verbatim former `docs/stdlib/globals.md`.
#[allow(dead_code)]
pub(super) const GLOBALS_MD: &str = r#"---
title: "Globals"
section: "Standard Library"
order: 1
---

# Globals

## Always Available

No import or qualification needed.

| Name | Signature | Description |
|------|-----------|-------------|
| `print` | `(a) -> () where a: Display` | Print a value without trailing newline |
| `println` | `(a) -> () where a: Display` | Print a value with trailing newline |
| `panic` | `(a) -> b where a: Display` | Crash with an error message |
| `Ok` | `(a) -> Result(a, e)` | Construct a success Result |
| `Err` | `(e) -> Result(a, e)` | Construct an error Result |
| `Some` | `(a) -> Option(a)` | Construct a present Option |
| `None` | `Option(a)` | The absent Option value (not a function) |

Additionally, five **type descriptors** are in the global namespace for use with
`json.parse_map` and similar type-directed APIs:

| Name | Description |
|------|-------------|
| `Int` | Integer type descriptor |
| `Float` | Float type descriptor |
| `ExtFloat` | Extended-float type descriptor (IEEE-754 `f64`, usable as map/set keys) |
| `String` | String type descriptor |
| `Bool` | Boolean type descriptor |

## Available After Import

These constructors become available after importing their respective modules.
No module qualification is needed once imported.

| Name | Signature | Import | Description |
|------|-----------|--------|-------------|
| `Stop` | `(a) -> Step(a)` | `import list` | Signal early termination in `list.fold_until` |
| `Continue` | `(a) -> Step(a)` | `import list` | Signal continuation in `list.fold_until` |
| `Message` | `(a) -> ChannelResult(a)` | `import channel` | Wraps a received channel value |
| `Closed` | `ChannelResult(a)` | `import channel` | Channel is closed |
| `Empty` | `ChannelResult(a)` | `import channel` | Channel buffer empty (non-blocking receive) |
| `Sent` | `ChannelResult(a)` | `import channel` | Result variant for a completed `channel.select` send arm |
| `Recv` | `(Channel(a)) -> ChannelOp(a)` | `import channel` | Build a receive arm for `channel.select` |
| `Send` | `(Channel(a), a) -> ChannelOp(a)` | `import channel` | Build a send arm for `channel.select` |
| `Monday`..`Sunday` | `Weekday` | `import time` | Day-of-week constructors |
| `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS` | `Method` | `import http` | HTTP method constructors |


## `print`

```
print(value: a) -> () where a: Display
```

Prints a value to stdout. Does not append a newline. Accepts a single value that
implements `Display`.

```silt
fn main() {
    print("hello ")
    print("world")
    -- output: hello world
}
```


## `println`

```
println(value: a) -> () where a: Display
```

Prints a value to stdout followed by a newline. Accepts a single value that
implements `Display`.

```silt
fn main() {
    println("hello, world")
    -- output: hello, world\n
}
```


## `panic`

```
panic(value: a) -> b where a: Display
```

Terminates execution with an error message. Accepts any value that implements
`Display`. The return type is polymorphic because `panic` never returns -- it
can appear anywhere a value is expected.

```silt
-- noexec
fn main() {
    panic("something went wrong")
    panic(42)  -- also valid
}
```


## `Ok`

```
Ok(value: a) -> Result(a, e)
```

Constructs a success variant of `Result`.

```silt
fn main() {
    let r = Ok(42)
    -- r is Result(Int, e)
}
```


## `Err`

```
Err(error: e) -> Result(a, e)
```

Constructs an error variant of `Result`.

```silt
fn main() {
    let r = Err("not found")
    -- r is Result(a, String)
}
```


## `Some`

```
Some(value: a) -> Option(a)
```

Constructs a present variant of `Option`.

```silt
fn main() {
    let x = Some(42)
    match x {
        Some(n) -> println(n)
        None -> println("nothing")
    }
}
```


## `None`

```
None : Option(a)
```

The absent variant of `Option`. This is a value, not a function.

```silt
import option
fn main() {
    let x = None
    println(option.is_none(x))  -- true
}
```


## `Stop`

```
Stop(value: a) -> Step(a)
```

Signals early termination from `list.fold_until`. The value becomes the final
accumulator result.

```silt
import list
fn main() {
    let capped_sum = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        match {
            acc + x > 6 -> Stop(acc)
            _ -> Continue(acc + x)
        }
    }
    println(capped_sum)  -- 6
}
```


## `Continue`

```
Continue(value: a) -> Step(a)
```

Signals continuation in `list.fold_until`. The value becomes the next
accumulator.


## `Message`

```
Message(value: a) -> ChannelResult(a)
```

Wraps a value received from a channel. Returned by `channel.receive` and
`channel.try_receive` when a value is available.

```silt
import channel
fn main() {
    let ch = channel.new(1)
    channel.send(ch, 42)
    when let Message(v) = channel.receive(ch) else { return }
    println(v)  -- 42
}
```


## `Closed`

```
Closed : ChannelResult(a)
```

Indicates the channel has been closed. Returned by `channel.receive` and
`channel.try_receive` when no more messages will arrive.


## `Empty`

```
Empty : ChannelResult(a)
```

Indicates the channel buffer is currently empty but not closed. Only returned by
`channel.try_receive` (the non-blocking variant).


## `Sent`

```
Sent : ChannelResult(a)
```

Indicates a successful send operation inside `channel.select`. When a select
arm is built with `Send(ch, value)` (a `ChannelOp(a)` value), the matching
tuple result is `(ch, Sent)` once that send completes. `Recv(ch)` arms still
produce `Message(v)` / `Closed`; `Sent` is the send-side counterpart to
`Message`. See `channel.select` in [channel / task](./channel-task.md) for
the mixed send/receive form.
"#;

/// Verbatim former `docs/stdlib/http.md`.
#[allow(dead_code)]
pub(super) const HTTP_MD: &str = r#"---
title: "http"
section: "Standard Library"
order: 14
---

# http

HTTP client and server. Included by default. Exclude with `--no-default-features` for WASM or minimal builds (networking functions will return a runtime error, but `http.segments` still works).

## Types

```silt
type Method { GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS }

type Request {
  method: Method,
  path: String,
  query: String,
  headers: Map(String, String),
  body: String,
}

type Response {
  status: Int,
  body: String,
  headers: Map(String, String),
}
```

`Method` variants are gated constructors -- using `GET`, `POST`, etc. requires `import http`.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `get` | `(String) -> Result(Response, HttpError)` | HTTP GET request |
| `request` | `(Method, String, String, Map(String, String)) -> Result(Response, HttpError)` | HTTP request with method, URL, body, headers |
| `serve` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server bound to `127.0.0.1` (loopback only) |
| `serve_all` | `(Int, Fn(Request) -> Response) -> ()` | Start a concurrent HTTP server bound to `0.0.0.0` (all interfaces) |
| `segments` | `(String) -> List(String)` | Split URL path into segments |
| `parse_query` | `(String) -> Map(String, List(String))` | Parse a URL query string into a multi-value map |

## Errors

`http.get` and `http.request` return `Result(Response, HttpError)`. Note
that a 4xx or 5xx HTTP response is an `Ok(Response)` — only failures
*before* a response lands (DNS, connection, TLS, protocol) become `Err`.
Servers that explicitly want to short-circuit on a non-2xx code can
construct `HttpStatusCode(status, body)` themselves; the stdlib does
not do that conversion for you. `HttpError` implements the built-in
`Error` trait, so `e.message()` always yields a rendered string.

| Variant | Fields | Meaning |
|---------|--------|---------|
| `HttpConnect(msg)` | `String` | TCP / DNS connect failure |
| `HttpTls(msg)` | `String` | TLS handshake / cert failure |
| `HttpTimeout` | — | request exceeded its deadline |
| `HttpInvalidUrl(url)` | `String` | URL did not parse |
| `HttpInvalidResponse(msg)` | `String` | response violated protocol |
| `HttpClosedEarly` | — | peer closed before response completed |
| `HttpStatusCode(status, body)` | `Int, String` | user-constructed for non-success codes |
| `HttpUnknown(msg)` | `String` | unclassified transport error |


## `http.get`

```
http.get(url: String) -> Result(Response, HttpError)
```

Makes an HTTP GET request. Returns `Ok(Response)` for any successful
connection (including 4xx/5xx status codes). Returns `Err(HttpError)`
for network errors (DNS failure, connection refused, timeout, TLS).

When called from a spawned task, `http.get` transparently yields to the
scheduler while the request is in flight. No API change is needed -- the
call site looks the same.

```silt
import http
import string
fn main() {
  match http.get("https://api.github.com/users/torvalds") {
    Ok(resp) -> println("Status: {resp.status}, body length: {string.length(resp.body)}")
    Err(HttpTimeout) -> println("timed out; retry later")
    Err(e) -> println("Network error: {e.message()}")
  }
}
```

Compose with `json.parse` and `?` for typed API responses. Since
`http.get` and `json.parse` return different error types, wrap each in
a local enum using `result.map_err` with a variant constructor as a
first-class `Fn`:

```silt
import http
import json
import result

type User { name: String, id: Int }

type FetchError {
  Network(HttpError),
  Parse(JsonError),
}

fn fetch_user(name: String) -> Result(User, FetchError) {
  let resp = http.get("https://api.example.com/users/{name}")
    |> result.map_err(Network)?
  json.parse(resp.body, User) |> result.map_err(Parse)
}
```


## `http.request`

```
http.request(method: Method, url: String, body: String, headers: Map(String, String)) -> Result(Response, HttpError)
```

Makes an HTTP request with full control over method, body, and headers. Use this for POST, PUT, DELETE, or any request that needs custom headers.

Like `http.get`, this transparently yields to the scheduler when called from
a spawned task.

```silt
-- POST with JSON body
let resp = http.request(
  POST,
  "https://api.example.com/users",
  json.stringify(#{"name": "Alice"}),
  #{"Content-Type": "application/json", "Authorization": "Bearer tok123"}
)?

-- DELETE
let resp = http.request(DELETE, "https://api.example.com/users/42", "", #{})?

-- GET with custom headers
let resp = http.request(GET, "https://api.example.com/data", "", #{"Accept": "text/plain"})?
```


## `http.serve`

```
http.serve(port: Int, handler: Fn(Request) -> Response) -> ()
```

Starts an HTTP server on the given port, **bound to `127.0.0.1` (loopback
only)**. This is the safe default: the listener is only reachable from the
same host, so a development server is not accidentally exposed to the
network. To accept connections from other machines, use
[`http.serve_all`](#httpserve_all).

Each incoming request is handled on its own thread with a fresh VM, so
multiple requests are processed concurrently. The accept loop runs on a
dedicated OS thread and does not block the scheduler. If a handler function
errors, the server returns a 500 response without crashing. The handler
receives a `Request` and must return a `Response`. The server runs forever
(stop with Ctrl-C).

Use pattern matching on `(req.method, segments)` for routing:

```silt
import http
import json

type User { id: Int, name: String }

fn main() {
  println("Listening on :8080")

  http.serve(8080, fn(req) {
    match (req.method, http.segments(req.path)) {
      (GET, []) ->
        Response { status: 200, body: "Hello!", headers: #{} }

      (GET, ["users", id]) ->
        Response { status: 200, body: "User {id}", headers: #{} }

      (POST, ["users"]) ->
        match json.parse(req.body, User) {
          Ok(user) -> Response {
            status: 201,
            body: json.stringify(user),
            headers: #{"Content-Type": "application/json"},
          }
          Err(e) -> Response { status: 400, body: e.message(), headers: #{} }
        }

      _ ->
        Response { status: 404, body: "Not found", headers: #{} }
    }
  })
}
```

Unsupported HTTP methods (e.g. TRACE) receive an automatic 405 response.


## `http.serve_all`

```
http.serve_all(port: Int, handler: Fn(Request) -> Response) -> ()
```

Identical to [`http.serve`](#httpserve) except the listener is bound to
`0.0.0.0`, so the server accepts connections from *any* network interface
(localhost, LAN, and public IPs if the host is routed).

**Security rationale.** The default `http.serve` binds to `127.0.0.1` so a
development server cannot be accidentally exposed to the network — a
common source of data leaks when a laptop joins an untrusted Wi-Fi, or a
container is run without explicit port firewalling. `http.serve_all` is
the explicit opt-in for the minority of cases where binding all interfaces
is actually what you want (deployment behind a reverse proxy, LAN-only
services, containers where loopback is bridged). The two variants
otherwise behave identically — same concurrency caps, same body-size
limits, same error handling.

```silt
import http

fn main() {
  -- Accept connections from anywhere. Make sure this is really what
  -- you want before shipping.
  http.serve_all(8080) { _req ->
    Response { status: 200, body: "Hello, world!", headers: #{} }
  }
}
```


## `http.segments`

```
http.segments(path: String) -> List(String)
```

Splits a URL path into non-empty segments. Useful for pattern-matched routing.

```silt
http.segments("/api/users/42")   -- ["api", "users", "42"]
http.segments("/")               -- []
http.segments("//foo//bar/")     -- ["foo", "bar"]
```

This function has no dependencies and works even with `--no-default-features`.


## `http.parse_query`

```
http.parse_query(query: String) -> Map(String, List(String))
```

Parses a URL query string into a map from key to a list of values. Repeated
keys accumulate into the same list in the order they appear, so a query like
`tag=a&tag=b` parses as `#{"tag": ["a", "b"]}`.

- A leading `?` is accepted and ignored.
- Percent escapes (`%HH`) in both keys and values are decoded. Invalid or
  truncated escapes cause a runtime error.
- Following the `application/x-www-form-urlencoded` convention, `+` decodes
  to a space in values.
- A key with no `=` (e.g. `flag&other=x`) is treated as having an empty
  string value: `#{"flag": [""], "other": ["x"]}`.
- Empty segments from leading, doubled, or trailing `&` are silently skipped.
- An empty input (or a bare `?`) returns the empty map.

```silt
import http

fn main() {
    http.parse_query("name=alice&tag=dev&tag=admin")
    -- #{"name": ["alice"], "tag": ["dev", "admin"]}

    http.parse_query("?q=hello%20world")
    -- #{"q": ["hello world"]}

    http.parse_query("")
    -- #{}
}
```

Like `http.segments`, this function has no network dependencies and works
with `--no-default-features`. Pair it with `req.query` in an `http.serve`
handler to route on query parameters.
"#;

/// Verbatim former `docs/stdlib/int-float.md`.
#[allow(dead_code)]
pub(super) const INT_FLOAT_MD: &str = r#"---
title: "int / float"
section: "Standard Library"
order: 6
---

# int

Functions for parsing, converting, and comparing integers.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Int) -> Int` | Absolute value |
| `clamp` | `(Int, Int, Int) -> Int` | Clamp value to `[lo, hi]` |
| `max` | `(Int, Int) -> Int` | Larger of two values |
| `min` | `(Int, Int) -> Int` | Smaller of two values |
| `parse` | `(String) -> Result(Int, ParseError)` | Parse string to integer |
| `to_float` | `(Int) -> Float` | Convert to float |
| `to_string` | `(Int) -> String` | Convert to string |


## `int.abs`

```
int.abs(n: Int) -> Int
```

Returns the absolute value. Runtime error if `n` is `Int` minimum
(`-9223372036854775808`) since the result cannot be represented.

```silt
import int
fn main() {
    println(int.abs(-42))  -- 42
    println(int.abs(7))    -- 7
}
```


## `int.clamp`

```
int.clamp(x: Int, lo: Int, hi: Int) -> Int
```

Returns `x` constrained to the inclusive range `[lo, hi]`: `lo` if
`x < lo`, `hi` if `x > hi`, otherwise `x`. Runtime error if `lo > hi`
(invalid bounds).

```silt
import int
fn main() {
    println(int.clamp(5, 0, 10))    -- 5
    println(int.clamp(-3, 0, 10))   -- 0
    println(int.clamp(42, 0, 10))   -- 10
}
```


## `int.max`

```
int.max(a: Int, b: Int) -> Int
```

Returns the larger of two integers.

```silt
import int
fn main() {
    println(int.max(3, 7))  -- 7
}
```


## `int.min`

```
int.min(a: Int, b: Int) -> Int
```

Returns the smaller of two integers.

```silt
import int
fn main() {
    println(int.min(3, 7))  -- 3
}
```


## `int.parse`

```
int.parse(s: String) -> Result(Int, ParseError)
```

Parses a string as an integer. Leading/trailing whitespace is trimmed. Returns
`Ok(n)` on success, `Err(ParseError)` on failure — a typed enum with variants
`ParseEmpty`, `ParseInvalidDigit(offset)`, `ParseOverflow`, and `ParseUnderflow`.
Match on the variant to handle specific cases, or fall back to `e.message()` for
a human-readable default.

```silt
import int
fn main() {
    match int.parse("42") {
        Ok(n) -> println(n)
        Err(e) -> println("parse error: {e.message()}")
    }

    -- Pattern-match on specific failure modes:
    match int.parse("") {
        Ok(_) -> ()
        Err(ParseEmpty) -> println("cannot parse empty input")
        Err(ParseInvalidDigit(i)) -> println("bad digit at byte {i}")
        Err(ParseOverflow) -> println("too large")
        Err(ParseUnderflow) -> println("too small")
    }
}
```


## `int.to_float`

```
int.to_float(n: Int) -> Float
```

Converts an integer to a float.

```silt
import int
fn main() {
    let f = int.to_float(42)
    println(f)  -- 42
}
```


## `int.to_string`

```
int.to_string(n: Int) -> String
```

Converts an integer to its string representation.

```silt
import int
fn main() {
    let s = int.to_string(42)
    println(s)  -- 42
}
```


---

# float

Functions for parsing, rounding, converting, and comparing floats.

> **Two-tier float system:** `Float` values are guaranteed finite — no NaN, no Infinity.
> Operations that may produce non-finite results (division, `sqrt`, `log`, `pow`, `exp`,
> `asin`, `acos`) return `ExtFloat` instead. Use the `else` keyword to narrow back to
> `Float` with a fallback: `a / b else 0.0`. Non-division arithmetic (`+`, `-`, `*`) on
> `Float` panics on overflow rather than producing Infinity.

> **Note:** `round`, `ceil`, and `floor` return `Float`, not `Int`. Use
> `float.to_int` to convert the result to an integer.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `abs` | `(Float) -> Float` | Absolute value |
| `ceil` | `(Float) -> Float` | Round up to nearest integer (as Float) |
| `clamp` | `(Float, Float, Float) -> Float` | Clamp value to `[lo, hi]` |
| `floor` | `(Float) -> Float` | Round down to nearest integer (as Float) |
| `is_finite` | `(ExtFloat) -> Bool` | True iff value is finite |
| `is_infinite` | `(ExtFloat) -> Bool` | True iff value is `±∞` |
| `is_nan` | `(ExtFloat) -> Bool` | True iff value is NaN |
| `max` | `(Float, Float) -> Float` | Larger of two values |
| `min` | `(Float, Float) -> Float` | Smaller of two values |
| `parse` | `(String) -> Result(Float, ParseError)` | Parse string to float |
| `round` | `(Float) -> Float` | Round to nearest integer (as Float) |
| `to_int` | `(Float) -> Int` | Truncate to integer |
| `to_string` | `(Float) -> String` | Shortest round-trippable representation |
| `to_string` | `(Float, Int) -> String` | Format with fixed decimal places |
| **Constants** | | |
| `float.max_value` | `Float` | Maximum finite value (`1.7976931348623157e+308`) |
| `float.min_value` | `Float` | Minimum finite value (`-1.7976931348623157e+308`) |
| `float.epsilon` | `Float` | Machine epsilon (`2.220446049250313e-16`) |
| `float.min_positive` | `Float` | Smallest positive normal (`2.2250738585072014e-308`) |
| `float.infinity` | `ExtFloat` | Positive infinity |
| `float.neg_infinity` | `ExtFloat` | Negative infinity |
| `float.nan` | `ExtFloat` | Not a Number |


## `float.abs`

```
float.abs(f: Float) -> Float
```

Returns the absolute value.

```silt
import float
fn main() {
    println(float.abs(-3.14))  -- 3.14
}
```


## `float.ceil`

```
float.ceil(f: Float) -> Float
```

Rounds up to the nearest integer, returned as a Float.

```silt
import float
fn main() {
    println(float.ceil(3.2))   -- 4
    println(float.ceil(-3.2))  -- -3
}
```


## `float.clamp`

```
float.clamp(x: Float, lo: Float, hi: Float) -> Float
```

Returns `x` constrained to the inclusive range `[lo, hi]`: `lo` if
`x < lo`, `hi` if `x > hi`, otherwise `x`. Runtime error if `lo > hi`
(invalid bounds).

Because `Float` is guaranteed finite, callers should not pass NaN here;
the output is **undefined for NaN inputs**. Use `float.is_nan` on an
`ExtFloat` first if you need to guard against this case.

```silt
import float
fn main() {
    println(float.clamp(0.5, 0.0, 1.0))   -- 0.5
    println(float.clamp(-0.2, 0.0, 1.0))  -- 0
    println(float.clamp(1.5, 0.0, 1.0))   -- 1
}
```


## `float.is_finite`

```
float.is_finite(x: ExtFloat) -> Bool
```

Returns `true` iff `x` is a finite number (not NaN, not `±∞`). Takes
`ExtFloat` because `Float` is guaranteed finite by construction — there
is no way to produce a non-finite `Float` that would make this predicate
interesting, so no `Float` overload is provided.

```silt
import float
fn main() {
    println(float.is_finite(float.nan))         -- false
    println(float.is_finite(float.infinity))    -- false
    println(float.is_finite(1.0 / 1.0))         -- true (division returns ExtFloat)
}
```


## `float.is_infinite`

```
float.is_infinite(x: ExtFloat) -> Bool
```

Returns `true` iff `x` is positive or negative infinity.

```silt
import float
fn main() {
    println(float.is_infinite(float.infinity))      -- true
    println(float.is_infinite(float.neg_infinity))  -- true
    println(float.is_infinite(float.nan))           -- false
}
```


## `float.is_nan`

```
float.is_nan(x: ExtFloat) -> Bool
```

Returns `true` iff `x` is NaN.

```silt
import float
fn main() {
    println(float.is_nan(float.nan))       -- true
    println(float.is_nan(float.infinity))  -- false
}
```


## `float.floor`

```
float.floor(f: Float) -> Float
```

Rounds down to the nearest integer, returned as a Float.

```silt
import float
fn main() {
    println(float.floor(3.9))   -- 3
    println(float.floor(-3.2))  -- -4
}
```


## `float.max`

```
float.max(a: Float, b: Float) -> Float
```

Returns the larger of two floats.

```silt
import float
fn main() {
    println(float.max(1.5, 2.5))  -- 2.5
}
```


## `float.min`

```
float.min(a: Float, b: Float) -> Float
```

Returns the smaller of two floats.

```silt
import float
fn main() {
    println(float.min(1.5, 2.5))  -- 1.5
}
```


## `float.parse`

```
float.parse(s: String) -> Result(Float, ParseError)
```

Parses a string as a float. Leading/trailing whitespace is trimmed. Returns
`Ok(f)` on success, `Err(ParseError)` on failure — the same typed enum
`int.parse` uses (`ParseEmpty`, `ParseInvalidDigit(offset)`, `ParseOverflow`,
`ParseUnderflow`). Strings like `"NaN"` and `"Infinity"` are rejected as
`ParseInvalidDigit(0)` since silt's `Float` is guaranteed finite.

```silt
import float
fn main() {
    match float.parse("3.14") {
        Ok(f) -> println(f)
        Err(e) -> println("error: {e.message()}")
    }
}
```


## `float.round`

```
float.round(f: Float) -> Float
```

Rounds to the nearest integer, returned as a Float. Ties round away from zero.

```silt
import float
fn main() {
    println(float.round(3.6))  -- 4
    println(float.round(3.4))  -- 3
}
```


## `float.to_int`

```
float.to_int(f: Float) -> Int
```

Truncates toward zero, converting to an integer. Returns a runtime error if
the value is NaN or Infinity.

```silt
import float
fn main() {
    println(float.to_int(3.9))   -- 3
    println(float.to_int(-3.9))  -- -3
}
```


## `float.to_string`

```
float.to_string(f: Float) -> String
float.to_string(f: Float, decimals: Int) -> String
```

Converts a float to its string representation. Accepts both `Float` and
`ExtFloat` values at runtime.

- **One-argument form:** returns the shortest round-trippable
  representation. Whole-number floats always include a decimal point
  (`3.0` rather than `3`) so the result parses back as a float.
- **Two-argument form:** formats with exactly `decimals` decimal
  places. `decimals` must be a non-negative `Int`.

```silt
import float
fn main() {
    -- 1-arg form: shortest round-trippable
    println(float.to_string(3.14159))     -- 3.14159
    println(float.to_string(42.0))        -- 42.0

    -- 2-arg form: fixed decimal places
    println(float.to_string(3.14159, 2))  -- 3.14
    println(float.to_string(42.0, 0))     -- 42
}
```


## Float Constants

| Constant | Type | Value |
|----------|------|-------|
| `float.max_value` | `Float` | `1.7976931348623157e+308` |
| `float.min_value` | `Float` | `-1.7976931348623157e+308` |
| `float.epsilon` | `Float` | `2.220446049250313e-16` |
| `float.min_positive` | `Float` | `2.2250738585072014e-308` |
| `float.infinity` | `ExtFloat` | Positive infinity |
| `float.neg_infinity` | `ExtFloat` | Negative infinity |
| `float.nan` | `ExtFloat` | Not a Number |

`float.max_value` and `float.min_value` are `Float` values (they're finite). The non-finite
constants are `ExtFloat` — use `else` to handle them if needed.
"#;

/// Verbatim former `docs/stdlib/io-fs.md`.
#[allow(dead_code)]
pub(super) const IO_FS_MD: &str = r#"---
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
"#;

/// Verbatim former `docs/stdlib/json.md`.
#[allow(dead_code)]
pub(super) const JSON_MD: &str = r#"---
title: "json"
section: "Standard Library"
order: 10
---

# json

Parse JSON strings into typed silt values and serialize values to JSON.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(String, type a) -> Result(a, JsonError)` | Parse JSON object into record |
| `parse_list` | `(String, type a) -> Result(List(a), JsonError)` | Parse JSON array into record list |
| `parse_map` | `(String, type v) -> Result(Map(String, v), JsonError)` | Parse JSON object into map |
| `pretty` | `(a) -> String` | Pretty-print value as JSON |
| `stringify` | `(a) -> String` | Serialize value as compact JSON |

## Errors

Every fallible `json.*` call returns `Result(T, JsonError)`. The `JsonError`
enum has four variants you can pattern-match on, or fall back to
`e.message()` when you just want a rendered string (`trait Error for
JsonError` is wired in):

| Variant | Fields | Meaning |
|---------|--------|---------|
| `JsonSyntax(msg, offset)` | `String`, `Int` | Malformed JSON at `offset` bytes |
| `JsonTypeMismatch(expected, actual)` | `String`, `String` | A field's JSON type did not match the target field type |
| `JsonMissingField(name)` | `String` | A required (non-`Option`) field was absent |
| `JsonUnknown(msg)` | `String` | Anything else (out-of-range numbers, internal failures) |

See [stdlib errors](errors.md) for the shared `Error` trait.


## `json.parse`

```
json.parse(s: String, type a) -> Result(a, JsonError)
```

Parses a JSON string into a record of type `a`. The type is passed as a `type`
parameter (see [Generics](../language/generics.md) — not a string). Fields are
matched by name; `Option` fields default to `None` if missing from the JSON.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) are
automatically parsed from ISO 8601 strings. `DateTime` fields also accept
timezone-aware formats (RFC 3339) — the offset is applied and the value is
stored as UTC:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD` | `"2024-03-15"` |
| `Time` | `HH:MM:SS`, `HH:MM` | `"14:30:00"` |
| `DateTime` | `YYYY-MM-DDTHH:MM:SS`, with optional `Z` or `±HH:MM` offset | `"2024-03-15T09:00:00+09:00"` |

```silt
import json
type User {
    name: String,
    age: Int,
}

fn main() {
    let input = """{"name": "Alice", "age": 30}"""
    match json.parse(input, User) {
        Ok(user) -> println(user.name)
        Err(e) -> println("Error: {e.message()}")
    }
}
```

Date/Time example:

```silt
import json
import time

type Event {
    name: String,
    date: Date,
}

fn main() -> Result(Unit, JsonError) {
    let e = json.parse("""{"name": "launch", "date": "2024-03-15"}""", Event)?
    println(e.date |> time.weekday)  -- Friday
    Ok(())
}
```


## `json.parse_list`

```
json.parse_list(s: String, type a) -> Result(List(a), JsonError)
```

Parses a JSON array where each element is a record of type `a`.

```silt
import json
import list
type Point {
    x: Int,
    y: Int,
}

fn main() {
    let input = """[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"""
    match json.parse_list(input, Point) {
        Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `json.parse_map`

```
json.parse_map(s: String, type v) -> Result(Map(String, v), JsonError)
```

Parses a JSON object into a `Map(String, v)`. The type is passed as a `type`
parameter (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
import json
import map
fn main() {
    let input = """{"x": 10, "y": 20}"""
    match json.parse_map(input, Int) {
        Ok(m) -> println(map.get(m, "x"))  -- Some(10)
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `json.pretty`

```
json.pretty(value: a) -> String
```

Serializes any value to a pretty-printed JSON string (with indentation and
newlines).

```silt
import json
fn main() {
    let data = #{"name": "silt", "version": "1.0"}
    println(json.pretty(data))
}
```


## `json.stringify`

```
json.stringify(value: a) -> String
```

Serializes any value to a compact JSON string.

```silt
import json
fn main() {
    let data = #{"key": [1, 2, 3]}
    println(json.stringify(data))
    -- {"key":[1,2,3]}
}
```
"#;

/// Verbatim former `docs/stdlib/list.md`.
#[allow(dead_code)]
pub(super) const LIST_MD: &str = r#"---
title: "list"
section: "Standard Library"
order: 2
---

# list

Functions for working with ordered, immutable lists (`List(a)`). Lists use
`[...]` literal syntax and support the range operator `1..5`.

See also [result / option](result-option.md) for the return types of
`find`, `head`, `last`, `get`, and `filter_map`; [stream](stream.md) for
channel-backed lazy pipelines over the same combinator names.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `all` | `(List(a), (a) -> Bool) -> Bool` | True if predicate holds for every element |
| `any` | `(List(a), (a) -> Bool) -> Bool` | True if predicate holds for at least one element |
| `append` | `(List(a), a) -> List(a)` | Add an element to the end |
| `concat` | `(List(a), List(a)) -> List(a)` | Concatenate two lists |
| `contains` | `(List(a), a) -> Bool` | Check if element is in list |
| `drop` | `(List(a), Int) -> List(a)` | Remove first n elements |
| `each` | `(List(a), (a) -> ()) -> ()` | Call function for each element (side effects) |
| `enumerate` | `(List(a)) -> List((Int, a))` | Pair each element with its index |
| `filter` | `(List(a), (a) -> Bool) -> List(a)` | Keep elements matching predicate |
| `filter_map` | `(List(a), (a) -> Option(b)) -> List(b)` | Filter and transform in one pass |
| `find` | `(List(a), (a) -> Bool) -> Option(a)` | First element matching predicate |
| `flat_map` | `(List(a), (a) -> List(b)) -> List(b)` | Map then flatten |
| `flatten` | `(List(List(a))) -> List(a)` | Flatten one level of nesting |
| `fold` | `(List(a), b, (b, a) -> b) -> b` | Reduce to a single value |
| `fold_until` | `(List(a), b, (b, a) -> Step(b)) -> b` | Fold with early termination |
| `get` | `(List(a), Int) -> Option(a)` | Element at index, or None |
| `group_by` | `(List(a), (a) -> k) -> Map(k, List(a))` | Group elements by key function |
| `head` | `(List(a)) -> Option(a)` | First element, or None |
| `index_of` | `(List(a), a) -> Option(Int)` | Index of first matching element, or None |
| `intersperse` | `(List(a), a) -> List(a)` | Insert separator between elements |
| `last` | `(List(a)) -> Option(a)` | Last element, or None |
| `length` | `(List(a)) -> Int` | Number of elements |
| `map` | `(List(a), (a) -> b) -> List(b)` | Transform each element |
| `max_by` | `(List(a), (a) -> b) -> Option(a)` | Element with largest key, or None |
| `min_by` | `(List(a), (a) -> b) -> Option(a)` | Element with smallest key, or None |
| `prepend` | `(List(a), a) -> List(a)` | Add an element to the front |
| `product` | `(List(Int)) -> Int` | Product of a list of ints (1 on empty) |
| `product_float` | `(List(Float)) -> Float` | Product of a list of floats (1.0 on empty) |
| `remove_at` | `(List(a), Int) -> List(a)` | Remove element at index (panics if out of range) |
| `reverse` | `(List(a)) -> List(a)` | Reverse element order |
| `scan` | `(List(a), b, (b, a) -> b) -> List(b)` | Prefix fold; returns all intermediate accumulators |
| `set` | `(List(a), Int, a) -> List(a)` | Return new list with element at index replaced |
| `sort` | `(List(a)) -> List(a)` | Sort in natural order |
| `sort_by` | `(List(a), (a) -> b) -> List(a)` | Sort by key function |
| `sum` | `(List(Int)) -> Int` | Sum a list of ints (0 on empty) |
| `sum_float` | `(List(Float)) -> Float` | Sum a list of floats (0.0 on empty) |
| `tail` | `(List(a)) -> List(a)` | All elements except the first |
| `take` | `(List(a), Int) -> List(a)` | Keep first n elements |
| `unfold` | `(a, (a) -> Option((b, a))) -> List(b)` | Build a list from a seed |
| `unique` | `(List(a)) -> List(a)` | Remove duplicates, preserving first occurrence |
| `zip` | `(List(a), List(b)) -> List((a, b))` | Pair elements from two lists |


## `list.all`

```
list.all(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for every element. Short-circuits on the
first `false`.

```silt
import list
fn main() {
    let all_even = list.all([2, 4, 6]) { x -> x % 2 == 0 }
    println(all_even)  -- true
}
```


## `list.any`

```
list.any(xs: List(a), f: (a) -> Bool) -> Bool
```

Returns `true` if `f` returns `true` for at least one element. Short-circuits on
the first `true`.

```silt
import list
fn main() {
    let has_even = list.any([1, 3, 4]) { x -> x % 2 == 0 }
    println(has_even)  -- true
}
```


## `list.append`

```
list.append(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the end.

```silt
import list
fn main() {
    let xs = [1, 2, 3] |> list.append(4)
    println(xs)  -- [1, 2, 3, 4]
}
```


## `list.concat`

```
list.concat(xs: List(a), ys: List(a)) -> List(a)
```

Concatenates two lists into a single list.

```silt
import list
fn main() {
    let joined = list.concat([1, 2], [3, 4])
    println(joined)  -- [1, 2, 3, 4]
}
```


## `list.contains`

```
list.contains(xs: List(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the list (by value equality).

```silt
import list
fn main() {
    println(list.contains([1, 2, 3], 2))  -- true
    println(list.contains([1, 2, 3], 5))  -- false
}
```


## `list.drop`

```
list.drop(xs: List(a), n: Int) -> List(a)
```

Returns the list without its first `n` elements. If `n >= length`, returns an
empty list. Negative `n` is a runtime error.

```silt
import list
fn main() {
    let tail = list.drop([1, 2, 3, 4, 5], 2)
    println(tail)  -- [3, 4, 5]
}
```


## `list.each`

```
list.each(xs: List(a), f: (a) -> ()) -> ()
```

Calls `f` for every element in the list. Used for side effects. Returns unit.

```silt
import list
fn main() {
    [1, 2, 3] |> list.each { x -> println(x) }
}
```


## `list.enumerate`

```
list.enumerate(xs: List(a)) -> List((Int, a))
```

Returns a list of `(index, element)` tuples, with indices starting at 0.

```silt
import list
fn main() {
    let pairs = list.enumerate(["a", "b", "c"])
    -- [(0, "a"), (1, "b"), (2, "c")]
    list.each(pairs) { (i, v) -> println("{i}: {v}") }
}
```


## `list.filter`

```
list.filter(xs: List(a), f: (a) -> Bool) -> List(a)
```

Returns a list containing only the elements for which `f` returns `true`.

```silt
import list
fn main() {
    let evens = [1, 2, 3, 4, 5] |> list.filter { x -> x % 2 == 0 }
    println(evens)  -- [2, 4]
}
```


## `list.filter_map`

```
list.filter_map(xs: List(a), f: (a) -> Option(b)) -> List(b)
```

Applies `f` to each element. Keeps the inner values from `Some` results and
discards `None` results. Combines filtering and mapping in one pass.

```silt
import int

import list
fn main() {
    let results = ["1", "abc", "3"] |> list.filter_map { s ->
        match int.parse(s) {
            Ok(n) -> Some(n * 10)
            Err(_) -> None
        }
    }
    println(results)  -- [10, 30]
}
```


## `list.find`

```
list.find(xs: List(a), f: (a) -> Bool) -> Option(a)
```

Returns `Some(element)` for the first element where `f` returns `true`, or
`None` if no match is found.

```silt
import list
fn main() {
    let first_gt_2 = list.find([1, 2, 3, 4]) { x -> x > 2 }
    println(first_gt_2)  -- Some(3)
}
```


## `list.flat_map`

```
list.flat_map(xs: List(a), f: (a) -> List(b)) -> List(b)
```

Maps each element to a list, then flattens the results into a single list.

```silt
import list
fn main() {
    let expanded = [1, 2, 3] |> list.flat_map { x -> [x, x * 10] }
    println(expanded)  -- [1, 10, 2, 20, 3, 30]
}
```


## `list.flatten`

```
list.flatten(xs: List(List(a))) -> List(a)
```

Flattens one level of nesting. Non-list elements are kept as-is.

```silt
import list
fn main() {
    let flat = list.flatten([[1, 2], [3], [4, 5]])
    println(flat)  -- [1, 2, 3, 4, 5]
}
```


## `list.fold`

```
list.fold(xs: List(a), init: b, f: (b, a) -> b) -> b
```

Reduces a list to a single value. Starts with `init`, then calls `f(acc, elem)`
for each element.

```silt
import list
fn main() {
    let sum = [1, 2, 3] |> list.fold(0) { acc, x -> acc + x }
    println(sum)  -- 6
}
```


## `list.fold_until`

```
list.fold_until(xs: List(a), init: b, f: (b, a) -> Step(b)) -> b
```

Like `fold`, but the callback returns `Continue(acc)` to keep going or
`Stop(value)` to terminate early.

```silt
import list
fn main() {
    -- Sum until we exceed 5
    let partial_sum = list.fold_until([1, 2, 3, 4, 5], 0) { acc, x ->
        let next = acc + x
        match {
            next > 5 -> Stop(acc)
            _ -> Continue(next)
        }
    }
    println(partial_sum)  -- 3
}
```


## `list.get`

```
list.get(xs: List(a), index: Int) -> Option(a)
```

Returns `Some(element)` at the given index, or `None` if out of bounds.
Negative indices are a runtime error -- use `list.last` for end access.

```silt
import list
fn main() {
    let xs = [10, 20, 30]
    println(list.get(xs, 1))   -- Some(20)
    println(list.get(xs, 10))  -- None
    -- list.get(xs, -1)        -- runtime error: negative index
}
```


## `list.group_by`

```
list.group_by(xs: List(a), f: (a) -> k) -> Map(k, List(a))
```

Groups elements by the result of applying `f`. Returns a map from keys to lists
of elements that produced that key.

```silt
import list
fn main() {
    let groups = [1, 2, 3, 4, 5, 6] |> list.group_by { x -> x % 2 }
    -- #{0: [2, 4, 6], 1: [1, 3, 5]}
}
```


## `list.head`

```
list.head(xs: List(a)) -> Option(a)
```

Returns `Some(first_element)` or `None` if the list is empty.

```silt
import list
fn main() {
    println(list.head([1, 2, 3]))  -- Some(1)
    println(list.head([]))         -- None
}
```


## `list.index_of`

```
list.index_of(xs: List(a), target: a) -> Option(Int)
```

Returns `Some(index)` of the first element equal to `target` (by value
equality), or `None` if no element matches.

```silt
import list
fn main() {
    println(list.index_of([10, 20, 30, 20], 20))  -- Some(1)
    println(list.index_of([10, 20, 30], 99))      -- None
}
```


## `list.intersperse`

```
list.intersperse(xs: List(a), sep: a) -> List(a)
```

Returns a new list with `sep` inserted between consecutive elements. Empty and
single-element inputs are returned unchanged.

```silt
import list
fn main() {
    println(list.intersperse([1, 2, 3], 0))  -- [1, 0, 2, 0, 3]
    println(list.intersperse([42], 0))       -- [42]
    println(list.intersperse([], 0))         -- []
}
```


## `list.last`

```
list.last(xs: List(a)) -> Option(a)
```

Returns `Some(last_element)` or `None` if the list is empty.

```silt
import list
fn main() {
    println(list.last([1, 2, 3]))  -- Some(3)
    println(list.last([]))         -- None
}
```


## `list.length`

```
list.length(xs: List(a)) -> Int
```

Returns the number of elements in the list.

```silt
import list
fn main() {
    println(list.length([1, 2, 3]))  -- 3
    println(list.length([]))         -- 0
}
```


## `list.map`

```
list.map(xs: List(a), f: (a) -> b) -> List(b)
```

Returns a new list with `f` applied to each element.

```silt
import list
fn main() {
    let doubled = [1, 2, 3] |> list.map { x -> x * 2 }
    println(doubled)  -- [2, 4, 6]
}
```


## `list.max_by`

```
list.max_by(xs: List(a), key: (a) -> b) -> Option(a)
```

Returns `Some(element)` whose `key` result is largest, or `None` if the list
is empty. On ties, returns the first element with the maximum key. Requires
`b` to support comparison (numbers, strings, etc.).

```silt
import list
import string
fn main() {
    let words = ["fig", "banana", "apple"]
    let longest = list.max_by(words) { w -> string.length(w) }
    println(longest)  -- Some(banana)
    println(list.max_by([], { x -> x }))  -- None
}
```


## `list.min_by`

```
list.min_by(xs: List(a), key: (a) -> b) -> Option(a)
```

Returns `Some(element)` whose `key` result is smallest, or `None` if the list
is empty. On ties, returns the first element with the minimum key. Requires
`b` to support comparison (numbers, strings, etc.).

```silt
import list
import string
fn main() {
    let words = ["banana", "fig", "apple"]
    let shortest = list.min_by(words) { w -> string.length(w) }
    println(shortest)  -- Some(fig)
    println(list.min_by([], { x -> x }))  -- None
}
```


## `list.prepend`

```
list.prepend(xs: List(a), elem: a) -> List(a)
```

Returns a new list with `elem` added at the front.

```silt
import list
fn main() {
    let xs = [2, 3] |> list.prepend(1)
    println(xs)  -- [1, 2, 3]
}
```


## `list.product`

```
list.product(xs: List(Int)) -> Int
```

Returns the product of all elements. Returns `1` on an empty list. Overflow
is a runtime error.

```silt
import list
fn main() {
    println(list.product([1, 2, 3, 4]))  -- 24
    println(list.product([]))            -- 1
}
```


## `list.product_float`

```
list.product_float(xs: List(Float)) -> Float
```

Like `product`, but for lists of floats. Returns `1.0` on an empty list.

```silt
import list
fn main() {
    println(list.product_float([1.5, 2.0, 4.0]))  -- 12
    println(list.product_float([]))               -- 1
}
```


## `list.remove_at`

```
list.remove_at(xs: List(a), index: Int) -> List(a)
```

Returns a new list with the element at `index` removed. Panics if the index is
out of bounds (matching `list.set`). Negative indices are a runtime error.

```silt
import list
fn main() {
    println(list.remove_at([10, 20, 30, 40], 1))  -- [10, 30, 40]
    -- list.remove_at([1, 2], 5)                   -- runtime error: out of bounds
}
```


## `list.reverse`

```
list.reverse(xs: List(a)) -> List(a)
```

Returns a new list with elements in reverse order.

```silt
import list
fn main() {
    println(list.reverse([1, 2, 3]))  -- [3, 2, 1]
}
```


## `list.set`

```
list.set(xs: List(a), index: Int, value: a) -> List(a)
```

Returns a new list with the element at `index` replaced by `value`. Panics if
the index is out of bounds. Negative indices are a runtime error.

```silt
import list
fn main() {
    let xs = list.set([10, 20, 30], 1, 99)
    println(xs)  -- [10, 99, 30]
}
```


## `list.sort`

```
list.sort(xs: List(a)) -> List(a)
```

Returns a new list sorted in natural (ascending) order.

```silt
import list
fn main() {
    println(list.sort([3, 1, 2]))  -- [1, 2, 3]
}
```


## `list.scan`

```
list.scan(xs: List(a), init: b, f: (b, a) -> b) -> List(b)
```

Like `fold`, but returns every intermediate accumulator rather than just the
final one. The result length is `length(xs) + 1`: the first element is `init`,
and each subsequent element is the accumulator after applying `f` to the next
input. This matches Haskell's `scanl` and Rust's `std::iter::successors`
(in spirit) — the convention is **inclusive of the initial value**.

```silt
import list
fn main() {
    -- Prefix sums: [0, 1, 3, 6, 10]
    let sums = list.scan([1, 2, 3, 4], 0) { acc, x -> acc + x }
    println(sums)
    println(list.scan([], 0) { acc, x -> acc + x })  -- [0]
}
```


## `list.sort_by`

```
list.sort_by(xs: List(a), key: (a) -> b) -> List(a)
```

Returns a new list sorted by the result of applying the key function to each
element.

```silt
import list
import string
fn main() {
    let words = ["banana", "fig", "apple"]
    let sorted = words |> list.sort_by { w -> string.length(w) }
    println(sorted)  -- [fig, apple, banana]
}
```


## `list.sum`

```
list.sum(xs: List(Int)) -> Int
```

Returns the sum of all elements. Returns `0` on an empty list. Overflow
is a runtime error.

```silt
import list
fn main() {
    println(list.sum([1, 2, 3, 4]))  -- 10
    println(list.sum([]))            -- 0
}
```


## `list.sum_float`

```
list.sum_float(xs: List(Float)) -> Float
```

Like `sum`, but for lists of floats. Returns `0.0` on an empty list.

```silt
import list
fn main() {
    println(list.sum_float([0.5, 1.5, 2.0]))  -- 4
    println(list.sum_float([]))               -- 0
}
```


## `list.tail`

```
list.tail(xs: List(a)) -> List(a)
```

Returns all elements except the first. Returns an empty list if the input is
empty.

```silt
import list
fn main() {
    println(list.tail([1, 2, 3]))  -- [2, 3]
    println(list.tail([]))         -- []
}
```


## `list.take`

```
list.take(xs: List(a), n: Int) -> List(a)
```

Returns the first `n` elements. If `n >= length`, returns the whole list.
Negative `n` is a runtime error.

```silt
import list
fn main() {
    println(list.take([1, 2, 3, 4, 5], 3))  -- [1, 2, 3]
}
```


## `list.unfold`

```
list.unfold(seed: a, f: (a) -> Option((b, a))) -> List(b)
```

Builds a list from a seed value. The function returns `Some((element, next_seed))`
to emit an element and continue, or `None` to stop.

```silt
import list
fn main() {
    let countdown = list.unfold(5) { n ->
        match {
            n <= 0 -> None
            _ -> Some((n, n - 1))
        }
    }
    println(countdown)  -- [5, 4, 3, 2, 1]
}
```


## `list.unique`

```
list.unique(xs: List(a)) -> List(a)
```

Removes duplicate elements, preserving the order of first occurrences.

```silt
import list
fn main() {
    println(list.unique([1, 2, 1, 3, 2]))  -- [1, 2, 3]
}
```


## `list.zip`

```
list.zip(xs: List(a), ys: List(b)) -> List((a, b))
```

Pairs up elements from two lists. Stops at the shorter list.

```silt
import list
fn main() {
    let pairs = list.zip([1, 2, 3], ["a", "b", "c"])
    println(pairs)  -- [(1, a), (2, b), (3, c)]
}
```
"#;

/// Verbatim former `docs/stdlib/map.md`.
#[allow(dead_code)]
pub(super) const MAP_MD: &str = r#"---
title: "map"
section: "Standard Library"
order: 4
---

# map

Functions for working with immutable, ordered maps (`Map(k, v)`). Maps use
`#{key: value}` literal syntax. Keys must satisfy the `Hash` trait constraint.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `contains` | `(Map(k, v), k) -> Bool` | Check if key exists |
| `delete` | `(Map(k, v), k) -> Map(k, v)` | Remove a key |
| `each` | `(Map(k, v), (k, v) -> ()) -> ()` | Iterate over all entries |
| `entries` | `(Map(k, v)) -> List((k, v))` | All key-value pairs as tuples |
| `filter` | `(Map(k, v), (k, v) -> Bool) -> Map(k, v)` | Keep entries matching predicate |
| `from_entries` | `(List((k, v))) -> Map(k, v)` | Build map from tuple list |
| `get` | `(Map(k, v), k) -> Option(v)` | Look up value by key |
| `keys` | `(Map(k, v)) -> List(k)` | All keys as a list |
| `length` | `(Map(k, v)) -> Int` | Number of entries |
| `map` | `(Map(k, v), (k, v) -> (k2, v2)) -> Map(k2, v2)` | Transform all entries |
| `merge` | `(Map(k, v), Map(k, v)) -> Map(k, v)` | Merge two maps (right wins) |
| `set` | `(Map(k, v), k, v) -> Map(k, v)` | Insert or update a key |
| `update` | `(Map(k, v), k, v, (v) -> v) -> Map(k, v)` | Update existing or insert default |
| `values` | `(Map(k, v)) -> List(v)` | All values as a list |


## `map.contains`

```
map.contains(m: Map(k, v), key: k) -> Bool
```

Returns `true` if the map has an entry for `key`.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    println(map.contains(m, "a"))  -- true
    println(map.contains(m, "z"))  -- false
}
```


## `map.delete`

```
map.delete(m: Map(k, v), key: k) -> Map(k, v)
```

Returns a new map with `key` removed. No-op if key does not exist.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let m2 = map.delete(m, "a")
    println(map.length(m2))  -- 1
}
```


## `map.each`

```
map.each(m: Map(k, v), f: (k, v) -> ()) -> ()
```

Calls `f` with each key-value pair. Used for side effects.

```silt
import map
fn main() {
    let m = #{"x": 10, "y": 20}
    map.each(m) { k, v -> println("{k} = {v}") }
}
```


## `map.entries`

```
map.entries(m: Map(k, v)) -> List((k, v))
```

Returns all key-value pairs as a list of tuples.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let pairs = map.entries(m)
    -- [("a", 1), ("b", 2)]
}
```


## `map.filter`

```
map.filter(m: Map(k, v), f: (k, v) -> Bool) -> Map(k, v)
```

Returns a new map containing only entries where `f` returns `true`.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2, "c": 3}
    let big = map.filter(m) { k, v -> v > 1 }
    -- #{"b": 2, "c": 3}
}
```


## `map.from_entries`

```
map.from_entries(entries: List((k, v))) -> Map(k, v)
```

Builds a map from a list of `(key, value)` tuples. Later entries overwrite
earlier ones with the same key.

```silt
import map
fn main() {
    let m = map.from_entries([("a", 1), ("b", 2)])
    println(m)  -- #{"a": 1, "b": 2}
}
```


## `map.get`

```
map.get(m: Map(k, v), key: k) -> Option(v)
```

Returns `Some(value)` if the key exists, or `None` otherwise.

```silt
import map
fn main() {
    let m = #{"name": "silt"}
    match map.get(m, "name") {
        Some(v) -> println(v)
        None -> println("not found")
    }
}
```


## `map.keys`

```
map.keys(m: Map(k, v)) -> List(k)
```

Returns all keys as a list, in sorted order.

```silt
import map
fn main() {
    let ks = map.keys(#{"b": 2, "a": 1})
    println(ks)  -- [a, b]
}
```


## `map.length`

```
map.length(m: Map(k, v)) -> Int
```

Returns the number of entries in the map.

```silt
import map
fn main() {
    println(map.length(#{"a": 1, "b": 2}))  -- 2
}
```


## `map.map`

```
map.map(m: Map(k, v), f: (k, v) -> (k2, v2)) -> Map(k2, v2)
```

Transforms each entry. The callback must return a `(key, value)` tuple.

```silt
import map
fn main() {
    let m = #{"a": 1, "b": 2}
    let doubled = map.map(m) { k, v -> (k, v * 2) }
    -- #{"a": 2, "b": 4}
}
```


## `map.merge`

```
map.merge(m1: Map(k, v), m2: Map(k, v)) -> Map(k, v)
```

Merges two maps. When both have the same key, the value from `m2` wins.

```silt
import map
fn main() {
    let a = #{"x": 1, "y": 2}
    let b = #{"y": 99, "z": 3}
    let merged = map.merge(a, b)
    -- #{"x": 1, "y": 99, "z": 3}
}
```


## `map.set`

```
map.set(m: Map(k, v), key: k, value: v) -> Map(k, v)
```

Returns a new map with the key set to value. Inserts if new, overwrites if
existing.

```silt
import map
fn main() {
    let m = #{"a": 1}
    let m2 = map.set(m, "b", 2)
    println(m2)  -- #{"a": 1, "b": 2}
}
```


## `map.update`

```
map.update(m: Map(k, v), key: k, default: v, f: (v) -> v) -> Map(k, v)
```

If `key` exists, applies `f` to the current value. If `key` does not exist,
applies `f` to `default`. Inserts the result.

```silt
import map
fn main() {
    let m = #{"a": 1}
    let m2 = map.update(m, "a", 0) { v -> v + 10 }
    let m3 = map.update(m2, "b", 0) { v -> v + 10 }
    -- m2 == #{"a": 11}
    -- m3 == #{"a": 11, "b": 10}
}
```


## `map.values`

```
map.values(m: Map(k, v)) -> List(v)
```

Returns all values as a list, in key-sorted order.

```silt
import map
fn main() {
    let vs = map.values(#{"a": 1, "b": 2})
    println(vs)  -- [1, 2]
}
```
"#;

/// Verbatim former `docs/stdlib/math.md`.
#[allow(dead_code)]
pub(super) const MATH_MD: &str = r#"---
title: "math"
section: "Standard Library"
order: 11
---

# math

Mathematical functions and constants. Functions that always produce finite results from
finite inputs return `Float`. Functions that may produce NaN or Infinity return `ExtFloat`
— use `else` to narrow back to `Float`.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `acos` | `(Float) -> ExtFloat` | Arccosine (radians) |
| `asin` | `(Float) -> ExtFloat` | Arcsine (radians) |
| `atan` | `(Float) -> Float` | Arctangent (radians) |
| `atan2` | `(Float, Float) -> Float` | Two-argument arctangent |
| `cos` | `(Float) -> Float` | Cosine |
| `e` | `Float` | Euler's number (2.71828...) |
| `exp` | `(Float) -> ExtFloat` | Exponential (e^x) |
| `log` | `(Float) -> ExtFloat` | Natural logarithm (ln) |
| `log10` | `(Float) -> ExtFloat` | Base-10 logarithm |
| `pi` | `Float` | Pi (3.14159...) |
| `pow` | `(Float, Float) -> ExtFloat` | Exponentiation |
| `random` | `() -> Float` | Random float in [0.0, 1.0) |
| `sin` | `(Float) -> Float` | Sine |
| `sqrt` | `(Float) -> ExtFloat` | Square root |
| `tan` | `(Float) -> Float` | Tangent |


## `math.acos`

```
math.acos(x: Float) -> ExtFloat
```

Returns the arccosine of `x` in radians. Returns `NaN` for inputs outside [-1, 1].
Use `else` to narrow:

```silt
import math
fn main() {
    let angle = math.acos(1.0) else 0.0
    println(angle)  -- 0  (silt's Float display drops the trailing `.0`
                    --    for integer-valued floats)
}
```


## `math.asin`

```
math.asin(x: Float) -> ExtFloat
```

Returns the arcsine of `x` in radians. Returns `NaN` for inputs outside [-1, 1].
Use `else` to narrow:

```silt
import math
fn main() {
    let angle = math.asin(1.0) else 0.0
    println(angle)  -- 1.5707... (pi/2)
}
```


## `math.atan`

```
math.atan(x: Float) -> Float
```

Returns the arctangent of `x` in radians.

```silt
import math
fn main() {
    println(math.atan(1.0))  -- 0.7853... (pi/4)
}
```


## `math.atan2`

```
math.atan2(y: Float, x: Float) -> Float
```

Returns the angle in radians between the positive x-axis and the point (x, y).
Handles all quadrants correctly.

```silt
import math
fn main() {
    println(math.atan2(1.0, 1.0))  -- 0.7853... (pi/4)
}
```


## `math.cos`

```
math.cos(x: Float) -> Float
```

Returns the cosine of `x` (in radians).

```silt
import math
fn main() {
    println(math.cos(0.0))       -- 1
    println(math.cos(math.pi))   -- -1
}
```


## `math.e`

```
math.e : Float
```

Euler's number, approximately 2.718281828459045. This is a constant, not a
function.

```silt
import math
fn main() {
    println(math.e)  -- 2.718281828459045
}
```


## `math.exp`

```
math.exp(x: Float) -> ExtFloat
```

Returns e raised to the power of `x`. May overflow to Infinity for large inputs.
Use `else` to narrow:

```silt
import math
fn main() {
    let e_val = math.exp(1.0) else 0.0
    println(e_val)  -- 2.718281828459045
}
```


## `math.log`

```
math.log(x: Float) -> ExtFloat
```

Returns the natural logarithm (base e) of `x`. Returns `-Infinity` for zero,
`NaN` for negative inputs. Use `else` to narrow:

```silt
import math
fn main() {
    let ln_e = math.log(math.e) else 0.0
    println(ln_e)  -- 1
}
```


## `math.log10`

```
math.log10(x: Float) -> ExtFloat
```

Returns the base-10 logarithm of `x`. Returns `-Infinity` for zero,
`NaN` for negative inputs. Use `else` to narrow:

```silt
import math
fn main() {
    let log_100 = math.log10(100.0) else 0.0
    println(log_100)  -- 2
}
```


## `math.pi`

```
math.pi : Float
```

Pi, approximately 3.141592653589793. This is a constant, not a function.

```silt
import math
fn main() {
    let circumference = 2.0 * math.pi * 5.0
    println(circumference)
}
```


## `math.pow`

```
math.pow(base: Float, exponent: Float) -> ExtFloat
```

Returns `base` raised to the power of `exponent`. Returns `ExtFloat` — may be
Infinity for large results. Use `else` to narrow:

```silt
import math
fn main() {
    let two_to_ten = math.pow(2.0, 10.0) else 0.0
    println(two_to_ten)  -- 1024
}
```


## `math.random`

```
math.random() -> Float
```

Returns a random `Float` in the range [0.0, 1.0). The result is always finite.

```silt
import math
fn main() {
    let r = math.random()
    println(r)  -- e.g. 0.7291035...
}
```


## `math.sin`

```
math.sin(x: Float) -> Float
```

Returns the sine of `x` (in radians).

```silt
import math
fn main() {
    println(math.sin(0.0))           -- 0
    println(math.sin(1.5707963))     -- 0.9999999999999997 (approximately 1.0, pi/2)
}
```


## `math.sqrt`

```
math.sqrt(x: Float) -> ExtFloat
```

Returns the square root of `x`. Returns `NaN` for negative inputs. Use `else`
to narrow:

```silt
import math
fn main() {
    let root = math.sqrt(4.0) else 0.0
    println(root)  -- 2
}
```


## `math.tan`

```
math.tan(x: Float) -> Float
```

Returns the tangent of `x` (in radians).

```silt
import math
fn main() {
    println(math.tan(0.0))           -- 0
    println(math.tan(0.7853982))     -- 1.0000000732051062 (approximately 1.0, pi/4)
}
```
"#;

/// Verbatim former `docs/stdlib/postgres.md`.
#[allow(dead_code)]
pub(super) const POSTGRES_MD: &str = r#"---
title: "postgres"
section: "Standard Library"
order: 19
---

# postgres (opt-in feature)

PostgreSQL client backed by an `r2d2`-managed connection pool.
Cooperatively yields on I/O so a silt task that calls `postgres.query`
parks until the result lands and other tasks keep running in the
meantime.

The `postgres` module is **not** built by default. Build silt with
`--features postgres` to enable it, or `--features postgres-tls` to add
`native-tls`-backed TLS support for `postgresql+tls://` URLs.

```sh
cargo build --release --features postgres
```

Pair the builtins below with a silt-side `pg.silt` package that
declares the companion types (`PgPool`, `PgTx`, `PgError`, `Value`,
`QueryResult`, `ExecResult`, `PgCursor`, and `Notification`). The
built-in functions reference those types by name; the typechecker
unifies them against whatever your `pg.silt` library defines.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `connect` | `(String) -> Result(PgPool, PgError)` | Open a connection pool from a `postgresql://` URL (uses r2d2 defaults) |
| `connect_with` | `(String, Map(String, Int)) -> Result(PgPool, PgError)` | Like `connect` with a tunable options bag (see [Connect options](#connect-options)) |
| `query` | `(PgPool \| PgTx, String, List(Value)) -> Result(QueryResult, PgError)` | Run a SELECT-style statement and materialize rows |
| `execute` | `(PgPool \| PgTx, String, List(Value)) -> Result(ExecResult, PgError)` | Run an INSERT/UPDATE/DELETE and return affected-row count |
| `transact` | `(PgPool, Fn(PgTx) -> Result(a, PgError)) -> Result(a, PgError)` | Pin a single connection for a transaction; callback runs inside BEGIN/COMMIT |
| `close` | `(PgPool) -> ()` | Drop the pool; future ops on it error |
| `stream` | `(PgPool \| PgTx, String, List(Value)) -> Result(Channel(Row), PgError)` | Stream rows through a bounded channel (backpressured) |
| `cursor` | `(PgTx, String, List(Value), Int) -> Result(PgCursor, PgError)` | Declare a server-side cursor with batch size |
| `cursor_next` | `(PgCursor) -> Result(List(Map(String, Value)), PgError)` | Fetch the next batch of rows from a cursor |
| `cursor_close` | `(PgCursor) -> Result((), PgError)` | Release a cursor and its underlying connection |
| `listen` | `(PgPool, String) -> Result(Channel(Notification), PgError)` | LISTEN on a channel; delivers async notifications |
| `notify` | `(PgPool \| PgTx, String, String) -> Result((), PgError)` | NOTIFY a channel with a payload |
| `uuidv7` | `() -> String` | Generate a time-ordered UUIDv7 (RFC 9562) |

## Cooperative I/O

Every fallible op above (except `uuidv7`) cooperates with silt's task
scheduler: when called from inside a `task.spawn`'d task, it submits
the request to silt's I/O pool and yields the task slot until the
response arrives. From silt's perspective the call looks synchronous.
Called from the main task it runs synchronously on the calling thread.

## Transactions

`postgres.transact` pins one pooled connection for the duration of the
callback, issues `BEGIN` up-front, and either `COMMIT`s on `Ok(_)` or
`ROLLBACK`s on `Err(_)` (or on panic). The callback receives a `PgTx`
handle; queries that should participate in the transaction must go
through that handle — calling `postgres.query(pool, ...)` with the
enclosing pool would pick a different connection and miss the
transaction entirely. Nested `postgres.transact` calls are rejected;
use `postgres.execute(tx, "SAVEPOINT ...")` manually instead.

## Streaming and cursors

`postgres.stream` returns a bounded `Channel` whose elements are
`Result(Map(String, Value), PgError)` rows. A background worker pumps
the cursor into the channel and closes it when the query completes
(or on error). Slow consumers backpressure the server side via the
channel capacity.

`postgres.cursor` is the lower-level primitive: it `DECLARE`s a
server-side cursor inside an open transaction and returns an opaque
`PgCursor` that `cursor_next` can repeatedly drain in batches. Always
call `cursor_close` (or let the transaction commit/rollback, which
cleans up).

## LISTEN / NOTIFY

`postgres.listen(pool, "channel_name")` returns a `Channel` that
delivers a `Notification` record for every NOTIFY on that PostgreSQL
channel. The underlying worker owns a dedicated connection, so LISTEN
does not consume a slot from the regular query pool.
`postgres.notify(target, channel, payload)` sends a single NOTIFY.

## Example

```text
-- Pair with a user-side `pg.silt` that declares the Value ADT
-- (VInt/VStr/VBool/VFloat/VNull/VList), PgPool, PgTx, PgError,
-- QueryResult, ExecResult, PgCursor, and Notification.
import pg
import postgres

fn main() {
  match postgres.connect("postgresql://localhost/app") {
    Ok(pool) -> {
      -- Transactional INSERT + SELECT.
      let result = postgres.transact(pool, fn(tx) {
        let _ = postgres.execute(
          tx,
          "INSERT INTO users (id, name) VALUES ($1, $2)",
          [VStr(postgres.uuidv7()), VStr("alice")],
        )?
        postgres.query(tx, "SELECT count(*) FROM users", [])
      })
      match result {
        Ok(rows) -> println("committed: {rows}")
        Err(PgTxnAborted) -> println("rolled back (txn aborted — retry)")
        Err(e) -> println("rolled back: {e.message()}")
      }
      postgres.close(pool)
    }
    Err(e) -> println("connect err: {e.message()}")
  }
}
```

## Notes

- The `Value` parameter ADT (`VInt`, `VStr`, `VBool`, `VFloat`, `VNull`,
  `VList`) is declared in the user's `pg.silt`, not here — the builtin
  module references it by name only.
- `postgres.uuidv7` produces a time-ordered UUID suitable for use as a
  primary key; collisions within the same millisecond are disambiguated
  with random bits per RFC 9562.
- `postgres-tls` pulls in `native-tls` / `postgres-native-tls` and
  therefore depends on the system TLS stack (OpenSSL / Schannel /
  SecureTransport depending on platform).

## TLS: secure-by-default

The `sslmode=` parameter in the connection URL controls certificate
verification. Silt explicitly **breaks with libpq defaults** here to
fail closed against surprise MITM paths:

| `sslmode=` | Encryption | Cert validity | Hostname check |
| --- | --- | --- | --- |
| *(omitted)* | **yes** | **yes** | **yes** (equivalent to `verify-full`) |
| `disable` | no | — | — |
| `prefer` / `allow` | opportunistic | no | no |
| `require` | yes | no | no (libpq-compatible encryption-only) |
| `verify-ca` | yes | yes | no |
| `verify-full` | yes | yes | yes |

**New default (silt 0.11+)**: a connection URL that omits `sslmode=`
entirely resolves to `verify-full`. This is a deliberate deviation
from libpq's historical `prefer`, which silently downgraded to
plaintext on handshake failure. A silt program whose URL is just
`postgres://user@host/db` now requires a valid, hostname-matching TLS
cert — or an explicit opt-out via `?sslmode=disable`.

If silt was built **without** the `postgres-tls` feature, a URL that
defaults to `verify-full` (or any `require` / `verify-*`) returns a
clear `ConnectionError` at connect time rather than silently using
plaintext.

**`sslmode=require` remains encryption-only**: when you explicitly
write `sslmode=require` in the URL, silt keeps libpq semantics
(encryption on, cert/hostname validation off). It is an explicit
opt-in to the weaker mode. Prefer `verify-full` unless you have a
concrete reason otherwise.

**Recommended**:
- `postgres://user:pw@host/db` (nothing after the path) — safe default.
- `postgres://user:pw@host/db?sslmode=verify-full` — explicit, same behaviour.
- `postgres://user:pw@host/db?sslmode=disable` — local dev / Unix socket.
- Use `verify-ca` only when DNS / hostname configuration makes
  `verify-full` impractical.

## Connect options

`postgres.connect_with(url, opts)` accepts a `Map(String, Int)` options
bag for tunables that don't belong in the URL. Unknown keys are
silently ignored so new knobs can be added without a breaking change.

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `max_pool_size` | `Int` (> 0) | r2d2 default (`10`) | Upper bound on the number of pooled connections. Raise for highly concurrent silt programs that otherwise block waiting for a free connection. |

```text
-- explicit 32-connection pool
let pool = postgres.connect_with(
  "postgres://app@db/app",
  #{"max_pool_size": 32}
)?

-- Same as postgres.connect(url):
let pool = postgres.connect_with("postgres://app@db/app", #{})?
```

## Error detail redaction

PostgreSQL error responses routinely embed user row values in their
`DETAIL:`, `WHERE:`, and `HINT:` follow-on fields — for example, a
UNIQUE violation reports `DETAIL: Key (email)=(alice@example.com)
already exists.`. A silt web handler that echoes the `Err(_)` value
into a 5xx response body would otherwise leak that email to
unauthenticated callers.

Silt strips those follow-on fields before the error crosses the VM
boundary into silt. The primary short message and SQLSTATE code
remain intact so callers can still pattern-match on the typed
`PgError` variants (e.g. `PgQuery(msg, sqlstate)` carries the five-
character SQLSTATE code so constraint-specific branches can match on
`"23505"` for unique violations, `"23503"` for FK violations, etc.).
If you need the full un-redacted text for diagnostics, log it on the
Rust side (e.g. via a custom embedder) — the silt-side `PgError` value
is intentionally scrubbed.

## PgError variants

All fallible `postgres.*` calls return `Result(T, PgError)`. The
variants (declared in silt's stdlib, no user `pg.silt` entries
needed) are:

| Variant | Fields | Raised for |
| --- | --- | --- |
| `PgConnect(msg)` | `String` | Pool checkout, URL parse, SQLSTATE class `08` |
| `PgTls(msg)` | `String` | TLS handshake / cert read / connector build |
| `PgAuthFailed(msg)` | `String` | SQLSTATE class `28` (invalid auth) |
| `PgQuery(msg, sqlstate)` | `String, String` | Any other DbError; `sqlstate` is the 5-char code |
| `PgTypeMismatch(col, expected, actual)` | `String, String, String` | Row decode failures |
| `PgNoSuchColumn(col)` | `String` | SQLSTATE `42703` (undefined_column) |
| `PgClosed` | — | Connection dropped mid-query |
| `PgTimeout` | — | SQLSTATE `57014` or transport timeout |
| `PgTxnAborted` | — | SQLSTATE `25P02` (in_failed_sql_transaction) |
| `PgUnknown(msg)` | `String` | Catch-all for shapes we can't classify |

`PgError` implements the stdlib `Error` trait, so if you don't want
to pattern-match on variants you can always call `err.message()` to
get a formatted user-friendly string.
"#;

/// Verbatim former `docs/stdlib/regex.md`.
#[allow(dead_code)]
pub(super) const REGEX_MD: &str = r#"---
title: "regex"
section: "Standard Library"
order: 9
---

# regex

Regular expression functions. Pattern strings use standard regex syntax.

Stdlib signatures return `Bool`, `Option`, `List`, or `String` — there is
no `Result` slot on any `regex.*` fn. A pattern that fails to compile
surfaces as a runtime error at the call site, not as an `Err`. A
`RegexError` enum (`RegexInvalidPattern(String, Int)`, `RegexTooBig`)
exists for user code that wants to model regex failures in its own
types, but no stdlib function produces it.

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
            println(list.get(groups, 1))  -- Some(user)
            println(list.get(groups, 2))  -- Some(host)
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
            println(map.get(groups, "user"))  -- Some(alice)
            println(map.get(groups, "host"))  -- Some(example)
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
    println(first)  -- Some(123)
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
    println(nums)  -- [1, 22, 333]
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
    println(replaced)  -- abc NUM def 456
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
    println(scrubbed)  -- abc NUM def NUM
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
import regex
import result

fn main() {
    let doubled = regex.replace_all_with("\\d+", "a1 b22 c333") { m ->
        let n = int.parse(m) |> result.unwrap_or(0)
        int.to_string(n * 2)
    }
    println(doubled)  -- a2 b44 c666
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
    println(parts)  -- [hello, world, silt]
}
```
"#;

/// Verbatim former `docs/stdlib/result-option.md`.
#[allow(dead_code)]
pub(super) const RESULT_OPTION_MD: &str = r#"---
title: "result / option"
section: "Standard Library"
order: 7
---

# result

Functions for transforming and querying `Result(a, e)` values without pattern
matching.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Result(a, e), (a) -> Result(b, e)) -> Result(b, e)` | Chain fallible operations |
| `flatten` | `(Result(Result(a, e), e)) -> Result(a, e)` | Remove one nesting level |
| `is_err` | `(Result(a, e)) -> Bool` | True if Err |
| `is_ok` | `(Result(a, e)) -> Bool` | True if Ok |
| `map_err` | `(Result(a, e), (e) -> f) -> Result(a, f)` | Transform the error |
| `map_ok` | `(Result(a, e), (a) -> b) -> Result(b, e)` | Transform the success value |
| `unwrap_or` | `(Result(a, e), a) -> a` | Extract value or use default |


## `result.flat_map`

```
result.flat_map(r: Result(a, e), f: (a) -> Result(b, e)) -> Result(b, e)
```

If `r` is `Ok(v)`, calls `f(v)` and returns its result. If `r` is `Err`,
returns the `Err` unchanged. Useful for chaining fallible operations.

```silt
import int

import result
fn main() {
    let r = Ok("42")
        |> result.flat_map { s -> int.parse(s) }
    println(r)  -- Ok(42)
}
```


## `result.flatten`

```
result.flatten(r: Result(Result(a, e), e)) -> Result(a, e)
```

Collapses a nested Result. `Ok(Ok(v))` becomes `Ok(v)`, `Ok(Err(e))` becomes
`Err(e)`, and `Err(e)` stays `Err(e)`.

```silt
import result
fn main() {
    println(result.flatten(Ok(Ok(42))))         -- Ok(42)
    println(result.flatten(Ok(Err("oops"))))    -- Err(oops)
}
```


## `result.is_err`

```
result.is_err(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Err`.

```silt
import result
fn main() {
    println(result.is_err(Err("fail")))  -- true
    println(result.is_err(Ok(42)))       -- false
}
```


## `result.is_ok`

```
result.is_ok(r: Result(a, e)) -> Bool
```

Returns `true` if the result is an `Ok`.

```silt
import result
fn main() {
    println(result.is_ok(Ok(42)))       -- true
    println(result.is_ok(Err("fail")))  -- false
}
```


## `result.map_err`

```
result.map_err(r: Result(a, e), f: (e) -> f) -> Result(a, f)
```

If `r` is `Err(e)`, returns `Err(f(e))`. If `r` is `Ok`, returns it unchanged.

```silt
import result
fn main() {
    let r = Err("not found") |> result.map_err { e -> "Error: {e}" }
    println(r)  -- Err(Error: not found)
}
```

Works well with a variant constructor as the mapping function. Silt
treats a one-field variant constructor as a first-class `Fn(e) -> Wrap`,
so `result.map_err(r, Wrap)` lifts a module-specific error into a
caller-owned enum without a closure. `?` binds looser than `|>`, so
a pipe followed by `?` composes without parentheses:

```silt
import io
import result

type AppError {
  IoWrap(IoError),
}

fn load(path: String) -> Result(String, AppError) {
    let raw = io.read_file(path) |> result.map_err(IoWrap)?
    Ok(raw)
}

fn main() {
    match load("missing.txt") {
        Ok(s) -> println(s)
        Err(IoWrap(e)) -> println(e.message())
    }
}
```


## `result.map_ok`

```
result.map_ok(r: Result(a, e), f: (a) -> b) -> Result(b, e)
```

If `r` is `Ok(v)`, returns `Ok(f(v))`. If `r` is `Err`, returns it unchanged.

```silt
import result
fn main() {
    let r = Ok(21) |> result.map_ok { n -> n * 2 }
    println(r)  -- Ok(42)
}
```


## `result.unwrap_or`

```
result.unwrap_or(r: Result(a, e), default: a) -> a
```

Returns the `Ok` value, or `default` if the result is `Err`.

```silt
import result
fn main() {
    println(result.unwrap_or(Ok(42), 0))        -- 42
    println(result.unwrap_or(Err("fail"), 0))    -- 0
}
```


---

# option

Functions for transforming and querying `Option(a)` values without pattern
matching.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `flat_map` | `(Option(a), (a) -> Option(b)) -> Option(b)` | Chain optional operations |
| `is_none` | `(Option(a)) -> Bool` | True if None |
| `is_some` | `(Option(a)) -> Bool` | True if Some |
| `map` | `(Option(a), (a) -> b) -> Option(b)` | Transform the inner value |
| `to_result` | `(Option(a), e) -> Result(a, e)` | Convert to Result with error value |
| `unwrap_or` | `(Option(a), a) -> a` | Extract value or use default |


## `option.flat_map`

```
option.flat_map(opt: Option(a), f: (a) -> Option(b)) -> Option(b)
```

If `opt` is `Some(v)`, calls `f(v)` and returns its result. If `opt` is `None`,
returns `None`.

```silt
import option
fn main() {
    let chained = Some(42) |> option.flat_map { n ->
        match {
            n > 0 -> Some(n * 2)
            _ -> None
        }
    }
    println(chained)  -- Some(84)
}
```


## `option.is_none`

```
option.is_none(opt: Option(a)) -> Bool
```

Returns `true` if the option is `None`.

```silt
import option
fn main() {
    println(option.is_none(None))      -- true
    println(option.is_none(Some(1)))   -- false
}
```


## `option.is_some`

```
option.is_some(opt: Option(a)) -> Bool
```

Returns `true` if the option is `Some`.

```silt
import option
fn main() {
    println(option.is_some(Some(1)))   -- true
    println(option.is_some(None))      -- false
}
```


## `option.map`

```
option.map(opt: Option(a), f: (a) -> b) -> Option(b)
```

If `opt` is `Some(v)`, returns `Some(f(v))`. If `opt` is `None`, returns `None`.

```silt
import option
fn main() {
    let doubled = Some(21) |> option.map { n -> n * 2 }
    println(doubled)  -- Some(42)
}
```


## `option.to_result`

```
option.to_result(opt: Option(a), error: e) -> Result(a, e)
```

Converts `Some(v)` to `Ok(v)` and `None` to `Err(error)`.

```silt
import option
fn main() {
    let r = option.to_result(Some(42), "missing")
    println(r)  -- Ok(42)

    let r2 = option.to_result(None, "missing")
    println(r2)  -- Err(missing)
}
```


## `option.unwrap_or`

```
option.unwrap_or(opt: Option(a), default: a) -> a
```

Returns the inner value if `Some`, otherwise returns `default`.

```silt
import option
fn main() {
    println(option.unwrap_or(Some(42), 0))  -- 42
    println(option.unwrap_or(None, 0))      -- 0
}
```
"#;

/// Verbatim former `docs/stdlib/set.md`.
#[allow(dead_code)]
pub(super) const SET_MD: &str = r#"---
title: "set"
section: "Standard Library"
order: 5
---

# set

Functions for working with immutable, ordered sets (`Set(a)`). Sets use `#[...]`
literal syntax and contain unique values.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `contains` | `(Set(a), a) -> Bool` | Check membership |
| `difference` | `(Set(a), Set(a)) -> Set(a)` | Elements in first but not second |
| `each` | `(Set(a), (a) -> ()) -> ()` | Iterate over all elements |
| `filter` | `(Set(a), (a) -> Bool) -> Set(a)` | Keep elements matching predicate |
| `fold` | `(Set(a), b, (b, a) -> b) -> b` | Reduce to a single value |
| `from_list` | `(List(a)) -> Set(a)` | Create set from list |
| `insert` | `(Set(a), a) -> Set(a)` | Add an element |
| `intersection` | `(Set(a), Set(a)) -> Set(a)` | Elements in both sets |
| `is_subset` | `(Set(a), Set(a)) -> Bool` | True if first is subset of second |
| `length` | `(Set(a)) -> Int` | Number of elements |
| `map` | `(Set(a), (a) -> b) -> Set(b)` | Transform each element |
| `new` | `() -> Set(a)` | Create an empty set |
| `remove` | `(Set(a), a) -> Set(a)` | Remove an element |
| `symmetric_difference` | `(Set(a), Set(a)) -> Set(a)` | Elements in exactly one of the two sets |
| `to_list` | `(Set(a)) -> List(a)` | Convert set to sorted list |
| `union` | `(Set(a), Set(a)) -> Set(a)` | Combine all elements |


## `set.contains`

```
set.contains(s: Set(a), elem: a) -> Bool
```

Returns `true` if `elem` is in the set.

```silt
import set
fn main() {
    let s = #[1, 2, 3]
    println(set.contains(s, 2))  -- true
    println(set.contains(s, 5))  -- false
}
```


## `set.difference`

```
set.difference(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in `a` but not in `b`.

```silt
import set
fn main() {
    let diff = set.difference(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(diff))  -- [1]
}
```


## `set.each`

```
set.each(s: Set(a), f: (a) -> ()) -> ()
```

Calls `f` for every element. Used for side effects.

```silt
import set
fn main() {
    set.each(#[1, 2, 3]) { x -> println(x) }
}
```


## `set.filter`

```
set.filter(s: Set(a), f: (a) -> Bool) -> Set(a)
```

Returns a new set containing only elements for which `f` returns `true`.

```silt
import set
fn main() {
    let evens = set.filter(#[1, 2, 3, 4]) { x -> x % 2 == 0 }
    println(set.to_list(evens))  -- [2, 4]
}
```


## `set.fold`

```
set.fold(s: Set(a), init: b, f: (b, a) -> b) -> b
```

Reduces the set to a single value. Iteration order is sorted.

```silt
import set
fn main() {
    let sum = set.fold(#[1, 2, 3], 0) { acc, x -> acc + x }
    println(sum)  -- 6
}
```


## `set.from_list`

```
set.from_list(xs: List(a)) -> Set(a)
```

Creates a set from a list, removing duplicates.

```silt
import set
fn main() {
    let s = set.from_list([1, 2, 2, 3])
    println(set.length(s))  -- 3
}
```


## `set.insert`

```
set.insert(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` added. No-op if already present.

```silt
import set
fn main() {
    let s = set.insert(#[1, 2], 3)
    println(set.to_list(s))  -- [1, 2, 3]
}
```


## `set.intersection`

```
set.intersection(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in both `a` and `b`.

```silt
import set
fn main() {
    let common = set.intersection(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(common))  -- [2, 3]
}
```


## `set.is_subset`

```
set.is_subset(a: Set(a), b: Set(a)) -> Bool
```

Returns `true` if every element of `a` is also in `b`.

```silt
import set
fn main() {
    println(set.is_subset(#[1, 2], #[1, 2, 3]))  -- true
    println(set.is_subset(#[1, 4], #[1, 2, 3]))  -- false
}
```


## `set.length`

```
set.length(s: Set(a)) -> Int
```

Returns the number of elements in the set.

```silt
import set
fn main() {
    println(set.length(#[1, 2, 3]))  -- 3
}
```


## `set.map`

```
set.map(s: Set(a), f: (a) -> b) -> Set(b)
```

Returns a new set with `f` applied to each element. The result set may be
smaller if `f` maps distinct elements to the same value.

```silt
import set
fn main() {
    let scaled = set.map(#[1, 2, 3]) { x -> x * 10 }
    println(set.to_list(scaled))  -- [10, 20, 30]
}
```


## `set.new`

```
set.new() -> Set(a)
```

Creates a new empty set.

```silt
import set
fn main() {
    let s = set.new()
    let s = set.insert(s, 42)
    println(set.length(s))  -- 1
}
```


## `set.remove`

```
set.remove(s: Set(a), elem: a) -> Set(a)
```

Returns a new set with `elem` removed. No-op if not present.

```silt
import set
fn main() {
    let s = set.remove(#[1, 2, 3], 2)
    println(set.to_list(s))  -- [1, 3]
}
```


## `set.symmetric_difference`

```
set.symmetric_difference(a: Set(a), b: Set(a)) -> Set(a)
```

Returns elements that are in exactly one of `a` or `b` — equivalent to
`(a - b) ∪ (b - a)`.

```silt
import set
fn main() {
    let diff = set.symmetric_difference(#[1, 2, 3], #[2, 3, 4])
    println(set.to_list(diff))  -- [1, 4]
}
```


## `set.to_list`

```
set.to_list(s: Set(a)) -> List(a)
```

Converts the set to a sorted list.

```silt
import set
fn main() {
    let xs = set.to_list(#[3, 1, 2])
    println(xs)  -- [1, 2, 3]
}
```


## `set.union`

```
set.union(a: Set(a), b: Set(a)) -> Set(a)
```

Returns a set containing all elements from both `a` and `b`.

```silt
import set
fn main() {
    let combined = set.union(#[1, 2], #[2, 3])
    println(set.to_list(combined))  -- [1, 2, 3]
}
```
"#;

/// Verbatim former `docs/stdlib/stream.md`.
#[allow(dead_code)]
pub(super) const STREAM_MD: &str = r#"---
title: "stream"
section: "Standard Library"
order: 18
---

# stream

A library of channel-backed sources, transforms, and sinks. Streams are
simply [`Channel`](channel-task.md) values used as data flows — the
underlying primitive is unchanged. Each transform spawns an internal pump
thread that reads its input channel, calls the user closure, and writes to
the output channel. Backpressure is provided by channel capacity (default
16; configurable via `stream.buffered`).

Sinks (`collect`, `fold`, `count`, etc.) drain a channel synchronously in
the calling task. Because every source and transform pump runs on a
dedicated OS thread (not a scheduler worker), sinks can safely block even
when called from a `task.spawn`'d task — producers keep making progress
regardless of scheduler state.

See also [io / fs](io-fs.md) for the underlying file operations behind
`file_chunks` / `file_lines`, [tcp](tcp.md) for `tcp_chunks` / `tcp_lines`,
and [channel / task](channel-task.md) for the primitive channel operations.

## Summary

### Sources

| Function | Signature | Description |
|----------|-----------|-------------|
| `from_list` | `(List(a)) -> Channel(a)` | Emit list elements then close |
| `from_range` | `(Int, Int) -> Channel(Int)` | Emit `lo..=hi` then close |
| `repeat` | `(a) -> Channel(a)` | Infinite — pair with `take` |
| `unfold` | `(a, (a) -> Option((b, a))) -> Channel(b)` | Generator (closes on `None`) |
| `file_chunks` | `(String, Int) -> Channel(Result(Bytes, IoError))` | Read file in chunks |
| `file_lines` | `(String) -> Channel(Result(String, IoError))` | Read file line-by-line |
| `tcp_chunks` | `(TcpStream, Int) -> Channel(Result(Bytes, TcpError))` | Read TCP in chunks |
| `tcp_lines` | `(TcpStream) -> Channel(Result(String, TcpError))` | Read TCP line-by-line |

### Transforms

| Function | Signature |
|----------|-----------|
| `map` | `(Channel(a), (a) -> b) -> Channel(b)` |
| `map_ok` | `(Channel(Result(a, e)), (a) -> b) -> Channel(Result(b, e))` |
| `filter` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `filter_ok` | `(Channel(Result(a, e)), (a) -> Bool) -> Channel(Result(a, e))` |
| `flat_map` | `(Channel(a), (a) -> List(b)) -> Channel(b)` |
| `take` | `(Channel(a), Int) -> Channel(a)` |
| `drop` | `(Channel(a), Int) -> Channel(a)` |
| `take_while` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `drop_while` | `(Channel(a), (a) -> Bool) -> Channel(a)` |
| `chunks` | `(Channel(a), Int) -> Channel(List(a))` |
| `scan` | `(Channel(a), b, (b, a) -> b) -> Channel(b)` |
| `dedup` | `(Channel(a)) -> Channel(a)` |
| `buffered` | `(Channel(a), Int) -> Channel(a)` |

### Combinators

| Function | Signature |
|----------|-----------|
| `merge` | `(List(Channel(a))) -> Channel(a)` |
| `concat` | `(List(Channel(a))) -> Channel(a)` |
| `zip` | `(Channel(a), Channel(b)) -> Channel((a, b))` |

### Sinks

| Function | Signature |
|----------|-----------|
| `collect` | `(Channel(a)) -> List(a)` |
| `fold` | `(Channel(a), b, (b, a) -> b) -> b` |
| `each` | `(Channel(a), (a) -> ()) -> ()` |
| `count` | `(Channel(a)) -> Int` |
| `first` | `(Channel(a)) -> Option(a)` |
| `last` | `(Channel(a)) -> Option(a)` |
| `write_to_file` | `(Channel(Bytes), String) -> Result((), IoError)` |
| `write_to_tcp` | `(Channel(Bytes), TcpStream) -> Result((), TcpError)` |

## Examples

### Three-step pipeline

```silt
import stream

fn main() {
  let squares = stream.from_range(1, 100)
    |> stream.filter(fn(n) { n % 2 == 1 })
    |> stream.map(fn(n) { n * n })
    |> stream.take(5)
    |> stream.collect
  println(squares)
}
```

### Generator via unfold

```silt
import stream

fn main() {
  -- Generate 1, 2, 3, 4, 5 then None.
  let xs = stream.collect(stream.unfold(1, fn(n) {
    match n > 5 {
      true -> None
      false -> Some((n, n + 1))
    }
  }))
  println(xs)
}
```

## Design notes

- **Streams are channels.** No new value type. `stream.collect(ch)` works
  on any `Channel`, not just streams produced by this module.
- **Backpressure is automatic.** When the output channel of a transform
  fills up, the pump thread sleeps briefly and retries — back-pressuring
  into the input channel by not consuming further messages.
- **Errors flow through the stream.** File sources emit
  `Channel(Result(_, IoError))`; TCP sources emit
  `Channel(Result(_, TcpError))`. Each chunk can fail independently;
  consumers pattern-match. Use `map_ok` / `filter_ok` to apply
  transformations only to `Ok` values, passing `Err(_)` through unchanged.
- **No async/await.** Everything runs on OS threads or the silt scheduler
  via the existing cooperative-I/O machinery.
- **`stream.repeat` is infinite.** Always pair it with `take`,
  `take_while`, or another bounded sink — `collect` on an unbounded
  stream will hang.

## Forward compatibility

Function names mirror what method-form dispatch (`s.map(f)`) would look
like once silt grows a `Stream` trait. Existing silt programs will
continue to compile and behave identically when that trait lands.
"#;

/// Verbatim former `docs/stdlib/string.md`.
#[allow(dead_code)]
pub(super) const STRING_MD: &str = r#"---
title: "string"
section: "Standard Library"
order: 3
---

# string

Functions for working with immutable strings. Strings use `"..."` literal syntax
with `{expr}` interpolation.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `char_code` | `(String) -> Int` | Unicode code point of first character |
| `chars` | `(String) -> List(String)` | Split string into single-character strings |
| `contains` | `(String, String) -> Bool` | Check if substring exists |
| `ends_with` | `(String, String) -> Bool` | Check suffix |
| `from` | `(a) -> String` | Convert any value to its display string |
| `from_char_code` | `(Int) -> String` | Character from Unicode code point |
| `index_of` | `(String, String) -> Option(Int)` | Character index of first occurrence |
| `byte_length` | `(String) -> Int` | Length in bytes |
| `is_alnum` | `(String) -> Bool` | All chars are alphanumeric |
| `is_alpha` | `(String) -> Bool` | All chars are alphabetic |
| `is_digit` | `(String) -> Bool` | All chars are ASCII digits |
| `is_empty` | `(String) -> Bool` | String has zero length |
| `is_lower` | `(String) -> Bool` | All chars are lowercase |
| `is_upper` | `(String) -> Bool` | All chars are uppercase |
| `is_whitespace` | `(String) -> Bool` | All chars are whitespace |
| `join` | `(List(String), String) -> String` | Join list with separator |
| `last_index_of` | `(String, String) -> Option(Int)` | Character index of last occurrence |
| `length` | `(String) -> Int` | Length in characters |
| `lines` | `(String) -> List(String)` | Split on `\n` (strips trailing `\r`, no empty final element) |
| `pad_left` | `(String, Int, String) -> String` | Pad to width on the left |
| `pad_right` | `(String, Int, String) -> String` | Pad to width on the right |
| `repeat` | `(String, Int) -> String` | Repeat string n times |
| `replace` | `(String, String, String) -> String` | Replace all occurrences |
| `slice` | `(String, Int, Int) -> String` | Substring by character indices |
| `split` | `(String, String) -> List(String)` | Split on separator |
| `split_at` | `(String, Int) -> (String, String)` | Split into two strings at character index |
| `starts_with` | `(String, String) -> Bool` | Check prefix |
| `starts_with_at` | `(String, Int, String) -> Bool` | Check prefix at a given character offset |
| `to_lower` | `(String) -> String` | Convert to lowercase |
| `to_upper` | `(String) -> String` | Convert to uppercase |
| `trim` | `(String) -> String` | Remove leading and trailing whitespace |
| `trim_end` | `(String) -> String` | Remove trailing whitespace |
| `trim_start` | `(String) -> String` | Remove leading whitespace |


## `string.char_code`

```
string.char_code(s: String) -> Int
```

Returns the Unicode code point of the first character. Panics on empty strings.

```silt
import string
fn main() {
    println(string.char_code("A"))  -- 65
}
```


## `string.chars`

```
string.chars(s: String) -> List(String)
```

Splits the string into a list of single-character strings.

```silt
import string
fn main() {
    println(string.chars("hi"))  -- [h, i]
}
```


## `string.contains`

```
string.contains(s: String, sub: String) -> Bool
```

Returns `true` if `sub` appears anywhere in `s`.

```silt
import string
fn main() {
    println(string.contains("hello world", "world"))  -- true
}
```


## `string.ends_with`

```
string.ends_with(s: String, suffix: String) -> Bool
```

Returns `true` if `s` ends with `suffix`.

```silt
import string
fn main() {
    println(string.ends_with("hello.silt", ".silt"))  -- true
}
```


## `string.from`

```
string.from(value: a) -> String
```

Converts any value to its display string representation. This is the
programmatic equivalent of string interpolation `"{value}"`.

```silt
import string
fn main() {
    println(string.from(42))        -- 42
    println(string.from(true))      -- true
    println(string.from([1, 2, 3])) -- [1, 2, 3]
}
```


## `string.from_char_code`

```
string.from_char_code(code: Int) -> String
```

Converts a Unicode code point to a single-character string. Panics on invalid
code points.

```silt
import string
fn main() {
    println(string.from_char_code(65))  -- A
}
```


## `string.index_of`

```
string.index_of(s: String, needle: String) -> Option(Int)
```

Returns `Some(index)` with the character index of the first occurrence of
`needle` in `s`, or `None` if not found.

```silt
import string
fn main() {
    println(string.index_of("hello", "ll"))  -- Some(2)
    println(string.index_of("hello", "z"))   -- None
}
```


## `string.last_index_of`

```
string.last_index_of(s: String, needle: String) -> Option(Int)
```

Returns `Some(index)` with the character index of the *last* occurrence of
`needle` in `s`, or `None` if not found. Counterpart to `string.index_of`,
using the same character-based indexing convention.

```silt
import string
fn main() {
    println(string.last_index_of("banana", "a"))  -- Some(5)
    println(string.last_index_of("banana", "z"))  -- None
}
```


## `string.is_alnum`

```
string.is_alnum(s: String) -> Bool
```

Returns `true` if all characters are alphanumeric. Returns `false` for empty
strings.

```silt
import string
fn main() {
    println(string.is_alnum("abc123"))  -- true
    println(string.is_alnum("abc!"))    -- false
    println(string.is_alnum(""))        -- false
}
```


## `string.is_alpha`

```
string.is_alpha(s: String) -> Bool
```

Returns `true` if all characters are alphabetic. Returns `false` for empty
strings.

```silt
import string
fn main() {
    println(string.is_alpha("hello"))   -- true
    println(string.is_alpha("abc123"))  -- false
    println(string.is_alpha(""))        -- false
}
```


## `string.is_digit`

```
string.is_digit(s: String) -> Bool
```

Returns `true` if all characters are ASCII digits (0-9). Returns `false`
for empty strings.

```silt
import string
fn main() {
    println(string.is_digit("123"))   -- true
    println(string.is_digit("12a"))   -- false
    println(string.is_digit(""))      -- false
}
```


## `string.is_empty`

```
string.is_empty(s: String) -> Bool
```

Returns `true` if the string has zero length.

```silt
import string
fn main() {
    println(string.is_empty(""))     -- true
    println(string.is_empty("hi"))   -- false
}
```


## `string.is_lower`

```
string.is_lower(s: String) -> Bool
```

Returns `true` if all characters are lowercase. Returns `false` for empty
strings.

```silt
import string
fn main() {
    println(string.is_lower("hello"))  -- true
    println(string.is_lower("Hello"))  -- false
    println(string.is_lower(""))       -- false
}
```


## `string.is_upper`

```
string.is_upper(s: String) -> Bool
```

Returns `true` if all characters are uppercase. Returns `false` for empty
strings.

```silt
import string
fn main() {
    println(string.is_upper("HELLO"))  -- true
    println(string.is_upper("Hello"))  -- false
    println(string.is_upper(""))       -- false
}
```


## `string.is_whitespace`

```
string.is_whitespace(s: String) -> Bool
```

Returns `true` if all characters are whitespace. Returns `false` for empty
strings.

```silt
import string
fn main() {
    println(string.is_whitespace("  \t"))  -- true
    println(string.is_whitespace(" a "))   -- false
    println(string.is_whitespace(""))      -- false
}
```


## `string.join`

```
string.join(parts: List(String), separator: String) -> String
```

Joins a list of strings with a separator between each pair.

```silt
import string
fn main() {
    let joined = string.join(["a", "b", "c"], ", ")
    println(joined)  -- a, b, c
}
```


## `string.byte_length`

```
string.byte_length(s: String) -> Int
```

Returns the length of the string in bytes (UTF-8 encoding). See also
`string.length` which counts characters.

```silt
import string
fn main() {
    println(string.byte_length("hello"))  -- 5
    println(string.byte_length("café"))   -- 5  (é is 2 bytes in UTF-8)
}
```


## `string.length`

```
string.length(s: String) -> Int
```

Returns the number of characters in the string. Use `string.byte_length` if
you need the size in bytes.

```silt
import string
fn main() {
    println(string.length("hello"))  -- 5
    println(string.length("café"))   -- 4  (4 characters, 5 bytes)
}
```


## `string.lines`

```
string.lines(s: String) -> List(String)
```

Splits `s` on `\n` newline characters. A trailing newline does *not* produce
an empty final element, so `"a\nb\n"` yields `["a", "b"]`. A trailing `\r`
on each line is stripped, which normalises `\r\n` line endings from Windows
sources.

```silt
import string
fn main() {
    println(string.lines("a\nb\nc"))      -- [a, b, c]
    println(string.lines("a\nb\n"))       -- [a, b]
    println(string.lines(""))             -- []
}
```

Input containing `\r\n` sequences (Windows line endings) has the trailing
`\r` on each line stripped automatically.


## `string.pad_left`

```
string.pad_left(s: String, width: Int, pad: String) -> String
```

Pads `s` on the left with the first character of `pad` until it reaches
`width`. Returns `s` unchanged if already at or beyond `width`.

```silt
import string
fn main() {
    println(string.pad_left("42", 5, "0"))  -- 00042
}
```


## `string.pad_right`

```
string.pad_right(s: String, width: Int, pad: String) -> String
```

Pads `s` on the right with the first character of `pad` until it reaches
`width`. Returns `s` unchanged if already at or beyond `width`.

```silt
import string
fn main() {
    println(string.pad_right("hi", 5, "."))  -- hi...
}
```


## `string.repeat`

```
string.repeat(s: String, n: Int) -> String
```

Returns the string repeated `n` times. `n` must be non-negative.

```silt
import string
fn main() {
    println(string.repeat("ab", 3))  -- ababab
}
```


## `string.replace`

```
string.replace(s: String, from: String, to: String) -> String
```

Replaces all occurrences of `from` with `to`.

```silt
import string
fn main() {
    println(string.replace("hello world", "world", "silt"))
    -- hello silt
}
```


## `string.slice`

```
string.slice(s: String, start: Int, end: Int) -> String
```

Returns the substring from character index `start` (inclusive) to `end`
(exclusive). Indices are clamped to the string length. Returns an empty string
if `start > end`. Negative indices are a runtime error.

```silt
import string
fn main() {
    println(string.slice("hello", 1, 4))  -- ell
}
```


## `string.split`

```
string.split(s: String, separator: String) -> List(String)
```

Splits the string on every occurrence of `separator`.

```silt
import string
fn main() {
    let parts = string.split("a,b,c", ",")
    println(parts)  -- [a, b, c]
}
```


## `string.split_at`

```
string.split_at(s: String, idx: Int) -> (String, String)
```

Splits `s` into `(left, right)` at character index `idx`. `idx == 0` yields
`("", s)` and `idx == length(s)` yields `(s, "")`. Panics on a negative index,
on an index past the end of the string, or on an index that does not fall on
a UTF-8 character boundary.

```silt
import string
fn main() {
    println(string.split_at("hello", 2))  -- (he, llo)
    println(string.split_at("hello", 0))  -- (, hello)
    println(string.split_at("hello", 5))  -- (hello, )
}
```


## `string.starts_with`

```
string.starts_with(s: String, prefix: String) -> Bool
```

Returns `true` if `s` starts with `prefix`.

```silt
import string
fn main() {
    println(string.starts_with("hello", "hel"))  -- true
}
```


## `string.starts_with_at`

```
string.starts_with_at(s: String, offset: Int, prefix: String) -> Bool
```

Returns `true` if `prefix` appears in `s` starting at character `offset`.
The offset is a character index (matching `string.index_of` and
`string.slice`). Out-of-range offsets (negative, or past the end of the
string) return `false` rather than panicking.

```silt
import string
fn main() {
    println(string.starts_with_at("hello", 2, "ll"))  -- true
    println(string.starts_with_at("hello", 2, "lx"))  -- false
    println(string.starts_with_at("hello", -1, "h"))  -- false
    println(string.starts_with_at("hello", 99, ""))   -- false
}
```


## `string.to_lower`

```
string.to_lower(s: String) -> String
```

Converts all characters to lowercase.

```silt
import string
fn main() {
    println(string.to_lower("HELLO"))  -- hello
}
```


## `string.to_upper`

```
string.to_upper(s: String) -> String
```

Converts all characters to uppercase.

```silt
import string
fn main() {
    println(string.to_upper("hello"))  -- HELLO
}
```


## `string.trim`

```
string.trim(s: String) -> String
```

Removes leading and trailing whitespace.

```silt
import string
fn main() {
    println(string.trim("  hello  "))  -- hello
}
```


## `string.trim_end`

```
string.trim_end(s: String) -> String
```

Removes trailing whitespace only.

```silt
import string
fn main() {
    println(string.trim_end("hello   "))  -- hello
}
```


## `string.trim_start`

```
string.trim_start(s: String) -> String
```

Removes leading whitespace only.

```silt
import string
fn main() {
    println(string.trim_start("   hello"))  -- hello
}
```
"#;

/// Verbatim former `docs/stdlib/tcp.md`.
#[allow(dead_code)]
pub(super) const TCP_MD: &str = r#"---
title: "tcp"
section: "Standard Library"
order: 17
---

# tcp

Raw TCP listeners and streams. Returns and consumes [`Bytes`](bytes.md) values
for binary I/O. Blocking operations cooperate with silt's task scheduler — a
silt task that calls `tcp.accept` or `tcp.read` yields its slot, letting other
tasks run, until the I/O completes.

The `tcp` feature is enabled by default. To build silt without it, disable
default features in your `Cargo.toml`.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `accept` | `(TcpListener) -> Result(TcpStream, TcpError)` | Wait for an incoming connection (cooperative I/O) |
| `close` | `(TcpStream) -> ()` | Mark the stream as closed; future ops error |
| `connect` | `(String) -> Result(TcpStream, TcpError)` | Open a TCP connection to `host:port` (cooperative I/O) |
| `listen` | `(String) -> Result(TcpListener, TcpError)` | Bind a TCP listener to `host:port` |
| `peer_addr` | `(TcpStream) -> Result(String, TcpError)` | Remote socket address (not yet implemented for trait-object stream handles; returns Err) |
| `read` | `(TcpStream, Int) -> Result(Bytes, TcpError)` | Read up to `max` bytes (cooperative) |
| `read_exact` | `(TcpStream, Int) -> Result(Bytes, TcpError)` | Read exactly `n` bytes (cooperative; loops) |
| `set_nodelay` | `(TcpStream, Bool) -> Result((), TcpError)` | Disable Nagle (not yet implemented for trait-object stream handles; returns Err) |
| `write` | `(TcpStream, Bytes) -> Result((), TcpError)` | Write the entire buffer and flush (cooperative) |

## Errors

Every fallible `tcp.*` call returns `Result(T, TcpError)`. Variants are
narrow by design — the socket failure space is small once you strip
out the OS-specific noise:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TcpConnect(msg)` | `String` | TCP / DNS connect failure |
| `TcpTls(msg)` | `String` | TLS handshake failure |
| `TcpClosed` | — | connection closed (broken pipe, peer reset) |
| `TcpTimeout` | — | op exceeded its deadline |
| `TcpUnknown(msg)` | `String` | unclassified socket failure |

`TcpError` implements the built-in `Error` trait, so `e.message()`
renders any variant as a string when you don't want to branch on it.

## Echo server example

```silt
import bytes
import tcp
import task
import time

fn main() {
  match tcp.listen("127.0.0.1:8080") {
    Ok(listener) -> {
      println("listening on 127.0.0.1:8080")
      loop {
        match tcp.accept(listener) {
          Ok(conn) -> {
            let _ = task.spawn(fn() {
              match tcp.read(conn, 4096) {
                Ok(buf) -> {
                  let _ = tcp.write(conn, buf)
                  tcp.close(conn)
                }
                Err(_) -> tcp.close(conn)
              }
            })
          }
          Err(e) -> println("accept error: {e.message()}")
        }
      }
    }
    Err(e) -> println("listen error: {e.message()}")
  }
}
```

## Cooperative I/O

`accept`, `connect`, `read`, `read_exact`, and `write` integrate with the silt
scheduler: when called inside a `task.spawn`'d task, they submit the I/O to
silt's thread pool and yield the task slot until the operation completes.
Other tasks run in the meantime. From silt's perspective the call looks
synchronous; under the hood it's cooperative.

When called from the main task (no `task.spawn`), the same operations run
synchronously on the calling thread.

## Stream lifetime

`TcpStream` and `TcpListener` are garbage-collected via `Arc` reference
counting. Dropping the last reference closes the underlying socket.
`tcp.close` is a defensive marker — it makes subsequent `read`/`write` calls
fail fast with a clear message instead of attempting I/O on a stream the user
has logically finished with.

## Notes

- `peer_addr` and `set_nodelay` currently return Err (they require unwrapping
  the trait-object stream). They will be wired up in a later release.
- silt does not use async/await. The scheduler does cooperative yielding via
  the same I/O pool used by `io.read_file`, `fs.list_dir`, etc.

## TLS (opt-in feature)

The `tcp-tls` Cargo feature adds TLS support via `rustls`. Build silt with
`--features tcp-tls` to enable.

| Function | Signature | Description |
|----------|-----------|-------------|
| `accept_tls` | `(TcpListener, Bytes, Bytes) -> Result(TcpStream, TcpError)` | Accept a connection and complete the TLS server handshake using the supplied PEM cert chain + key |
| `accept_tls_mtls` | `(TcpListener, Bytes, Bytes, Bytes) -> Result(TcpStream, TcpError)` | Like `accept_tls`, but also requires the client to present a cert chaining to the supplied CA PEM bundle (mutual TLS) |
| `connect_tls` | `(String, String) -> Result(TcpStream, TcpError)` | Open a TCP connection then complete the TLS client handshake against `hostname` |

Returned `TcpStream` handles are interchangeable with plain TCP streams —
`tcp.read`, `tcp.write`, and `tcp.close` work identically. Trust anchors
for `connect_tls` come from the `webpki-roots` crate (Mozilla CA bundle).
Authentication is delegated to your system: silt does not add a separate
credential layer.

```text
import bytes
import tcp

fn main() {
  -- Open a TLS-protected connection and echo a small payload.
  -- (Build silt with `--features tcp-tls` for these functions.)
  match tcp.connect_tls("example.com:443", "example.com") {
    Ok(conn) -> {
      let _ = tcp.write(conn, bytes.from_string("hello"))
      tcp.close(conn)
    }
    Err(e) -> println("connect_tls err: {e.message()}")
  }
}
```

### Mutual TLS (mTLS)

`accept_tls_mtls` adds client-certificate verification on top of
`accept_tls`. The fourth argument is a PEM-encoded bundle of CA
certificates — every connecting client must present a certificate that
chains to one of those CAs, or the TLS handshake fails and the call
returns `Err(TcpTls(msg))`. This is appropriate for service-to-service
APIs, internal mesh traffic, and any flow where you want cryptographic
client identity rather than bearer tokens.

Under the hood the server uses rustls'
`WebPkiClientVerifier::builder(roots).build()`, which requires
authentication by default (anonymous clients are rejected).

```text
import bytes
import io
import tcp

fn main() {
  -- Load the server identity and the CA bundle that signs your
  -- clients. (Build silt with `--features tcp-tls` for this function.)
  match io.read_file("server.crt") {
    Ok(cert) -> match io.read_file("server.key") {
      Ok(key) -> match io.read_file("clients-ca.crt") {
        Ok(client_ca) -> match tcp.listen("0.0.0.0:8443") {
          Ok(listener) -> match tcp.accept_tls_mtls(listener, cert, key, client_ca) {
            Ok(conn) -> {
              -- Peer is authenticated by cert at this point.
              let _ = tcp.write(conn, bytes.from_string("hello, authenticated client"))
              tcp.close(conn)
            }
            Err(e) -> println("mTLS handshake failed: {e.message()}")
          }
          Err(e) -> println("listen err: {e.message()}")
        }
        Err(e) -> println("ca load err: {e.message()}")
      }
      Err(e) -> println("key load err: {e.message()}")
    }
    Err(e) -> println("cert load err: {e.message()}")
  }
}
```
"#;

/// Verbatim former `docs/stdlib/test.md`.
#[allow(dead_code)]
pub(super) const TEST_MD: &str = r#"---
title: "test"
section: "Standard Library"
order: 15
---

# test

Assertion functions for test scripts. Each accepts an optional trailing `String`
message argument.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `assert` | `(Bool, String?) -> ()` | Assert value is truthy |
| `assert_eq` | `(a, a, String?) -> ()` | Assert two values are equal |
| `assert_ne` | `(a, a, String?) -> ()` | Assert two values are not equal |


## `test.assert`

```
test.assert(condition: Bool) -> ()
test.assert(condition: Bool, message: String) -> ()
```

Panics if `condition` is `false`. The optional message is included in the error.

```silt
import test
fn main() {
    test.assert(1 + 1 == 2)
    test.assert(1 + 1 == 2, "math should work")
}
```


## `test.assert_eq`

```
test.assert_eq(left: a, right: a) -> ()
test.assert_eq(left: a, right: a, message: String) -> ()
```

Panics if `left != right`, displaying both values.

```silt
import test
import list
fn main() {
    test.assert_eq(list.length([1, 2, 3]), 3)
    test.assert_eq(1 + 1, 2, "addition")
}
```


## `test.assert_ne`

```
test.assert_ne(left: a, right: a) -> ()
test.assert_ne(left: a, right: a, message: String) -> ()
```

Panics if `left == right`, displaying both values.

```silt
import test
fn main() {
    test.assert_ne("hello", "world")
}
```
"#;

/// Verbatim former `docs/stdlib/time.md`.
#[allow(dead_code)]
pub(super) const TIME_MD: &str = r#"---
title: "time"
section: "Standard Library"
order: 13
---

# time

Dates, times, instants, durations, formatting, parsing, and arithmetic. All values are immutable. Nanosecond precision throughout.

## Types

```silt
type Instant  { epoch_ns: Int }                           -- point on the UTC timeline (ns since Unix epoch)
type Date     { year: Int, month: Int, day: Int }          -- calendar date, no time or zone
type Time     { hour: Int, minute: Int, second: Int, ns: Int }  -- wall clock time, no date or zone
type DateTime { date: Date, time: Time }                   -- date + time, no zone
type Duration { ns: Int }                                  -- fixed elapsed time in nanoseconds
type Weekday  { Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday }
```

`Date`, `Time`, and `DateTime` display as ISO 8601 in string interpolation.
`Duration` displays in human-readable form (`2h30m15s`, `500ms`, `42ns`).
Comparison operators (`<`, `>`, `==`) work correctly on all time types.

## Errors

`time.date`, `time.time`, `time.parse`, and `time.parse_date` return
`Result(T, TimeError)`. The enum is intentionally small — calendar
validation and format parsing are the only structural failure modes:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TimeParseFormat(msg)` | `String` | pattern did not match input |
| `TimeOutOfRange(msg)` | `String` | field out of valid range (e.g. `month=13`) |

`TimeError` implements the built-in `Error` trait, so `e.message()`
yields a rendered string when variant branching isn't needed.

## Summary

| Name | Signature | Description |
|------|-----------|-------------|
| `now` | `() -> Instant` | Current UTC time as nanosecond epoch |
| `today` | `() -> Date` | Current local date |
| `date` | `(Int, Int, Int) -> Result(Date, TimeError)` | Validated date from year, month, day |
| `time` | `(Int, Int, Int) -> Result(Time, TimeError)` | Validated time from hour, min, sec (ns=0) |
| `datetime` | `(Date, Time) -> DateTime` | Combine date and time (infallible) |
| `to_datetime` | `(Instant, Int) -> DateTime` | Convert instant to local datetime with UTC offset in minutes |
| `to_instant` | `(DateTime, Int) -> Instant` | Convert local datetime to instant with UTC offset in minutes |
| `to_utc` | `(Instant) -> DateTime` | Convert instant to UTC datetime (shorthand for offset=0) |
| `from_utc` | `(DateTime) -> Instant` | Convert UTC datetime to instant (shorthand for offset=0) |
| `format` | `(DateTime, String) -> String` | Format datetime with strftime pattern |
| `format_date` | `(Date, String) -> String` | Format date with strftime pattern |
| `parse` | `(String, String) -> Result(DateTime, TimeError)` | Parse string into datetime with strftime pattern |
| `parse_date` | `(String, String) -> Result(Date, TimeError)` | Parse string into date with strftime pattern |
| `add_days` | `(Date, Int) -> Date` | Add/subtract days from a date |
| `add_months` | `(Date, Int) -> Date` | Add/subtract months, clamping to end-of-month |
| `add` | `(Instant, Duration) -> Instant` | Add duration to an instant |
| `since` | `(Instant, Instant) -> Duration` | Signed duration between two instants (to − from) |
| `hours` | `(Int) -> Duration` | Create duration from hours |
| `minutes` | `(Int) -> Duration` | Create duration from minutes |
| `seconds` | `(Int) -> Duration` | Create duration from seconds |
| `ms` | `(Int) -> Duration` | Create duration from milliseconds |
| `micros` | `(Int) -> Duration` | Create duration from microseconds |
| `nanos` | `(Int) -> Duration` | Create duration from nanoseconds |
| `weekday` | `(Date) -> Weekday` | Day of the week |
| `days_between` | `(Date, Date) -> Int` | Signed number of days between two dates |
| `days_in_month` | `(Int, Int) -> Int` | Days in month for given year and month |
| `is_leap_year` | `(Int) -> Bool` | Check if a year is a leap year |
| `sleep` | `(Duration) -> ()` | Fiber-aware sleep |


## `time.now`

```
time.now() -> Instant
```

Returns the current UTC time as nanoseconds since the Unix epoch (1970-01-01T00:00:00Z).

```silt
import time
fn main() {
    let t = time.now()
    println(t.epoch_ns)  -- 1775501213453369259
}
```


## `time.today`

```
time.today() -> Date
```

Returns the current date in the system's local timezone.

```silt
import time
fn main() {
    println(time.today())  -- 2026-04-06
}
```


## `time.date`

```
time.date(year: Int, month: Int, day: Int) -> Result(Date, TimeError)
```

Creates a validated `Date`. Returns `Err` for invalid dates.

```silt
import time
fn main() {
    println(time.date(2024, 3, 15))   -- Ok(2024-03-15)
    println(time.date(2024, 2, 29))   -- Ok(2024-02-29)  (leap year)
    println(time.date(2024, 13, 1))   -- Err(TimeOutOfRange(invalid date: 2024-13-1))
}
```


## `time.time`

```
time.time(hour: Int, min: Int, sec: Int) -> Result(Time, TimeError)
```

Creates a validated `Time` with `ns` set to 0. Returns `Err` for invalid times.

```silt
import time
fn main() {
    println(time.time(14, 30, 0))  -- Ok(14:30:00)
    println(time.time(25, 0, 0))   -- Err(TimeOutOfRange(invalid time: 25:0:0))
}
```


## `time.datetime`

```
time.datetime(date: Date, time: Time) -> DateTime
```

Combines a `Date` and `Time` into a `DateTime`. Infallible since both inputs are already validated.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let d = time.date(2024, 6, 15)?
    let t = time.time(9, 30, 0)?
    println(time.datetime(d, t))  -- 2024-06-15T09:30:00
    Ok(())
}
```


## `time.to_datetime`

```
time.to_datetime(instant: Instant, offset_minutes: Int) -> DateTime
```

Converts an `Instant` to a `DateTime` by applying a UTC offset in minutes.

```silt
import time
fn main() {
    let now = time.now()
    let tokyo = now |> time.to_datetime(540)    -- UTC+9:00
    let india = now |> time.to_datetime(330)    -- UTC+5:30
    println(tokyo)
    println(india)
}
```


## `time.to_instant`

```
time.to_instant(datetime: DateTime, offset_minutes: Int) -> Instant
```

Converts a local `DateTime` to an `Instant` by subtracting the UTC offset.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let dt = time.datetime(time.date(2024, 1, 1)?, time.time(0, 0, 0)?)
    let instant = time.to_instant(dt, 0)
    println(instant.epoch_ns)
    Ok(())
}
```


## `time.to_utc`

```
time.to_utc(instant: Instant) -> DateTime
```

Shorthand for `time.to_datetime(instant, 0)`.

```silt
import time
fn main() {
    println(time.now() |> time.to_utc)  -- 2026-04-06T18:46:09.005723612
}
```


## `time.from_utc`

```
time.from_utc(datetime: DateTime) -> Instant
```

Shorthand for `time.to_instant(datetime, 0)`.

```silt
import time
fn main() {
    let dt = time.now() |> time.to_utc
    let back = dt |> time.from_utc
    println(back.epoch_ns)
}
```


## `time.format`

```
time.format(datetime: DateTime, pattern: String) -> String
```

Formats a `DateTime` using strftime patterns. Supported: `%Y %m %d %H %M %S %f %A %a %B %b %%`.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let dt = time.datetime(time.date(2024, 12, 25)?, time.time(18, 0, 0)?)
    println(dt |> time.format("%A, %B %d, %Y at %H:%M"))
    -- Wednesday, December 25, 2024 at 18:00
    Ok(())
}
```


## `time.format_date`

```
time.format_date(date: Date, pattern: String) -> String
```

Formats a `Date` using strftime patterns.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let d = time.date(2024, 6, 15)?
    println(d |> time.format_date("%d/%m/%Y"))  -- 15/06/2024
    Ok(())
}
```


## `time.parse`

```
time.parse(s: String, pattern: String) -> Result(DateTime, TimeError)
```

Parses a string into a `DateTime` using a strftime pattern.

```silt
import time
fn main() {
    let dt = time.parse("2024-07-04 12:00:00", "%Y-%m-%d %H:%M:%S")
    println(dt)  -- Ok(2024-07-04T12:00:00)
}
```


## `time.parse_date`

```
time.parse_date(s: String, pattern: String) -> Result(Date, TimeError)
```

Parses a string into a `Date` using a strftime pattern.

```silt
import time
fn main() {
    let d = time.parse_date("2024-07-04", "%Y-%m-%d")
    println(d)  -- Ok(2024-07-04)
}
```


## `time.add_days`

```
time.add_days(date: Date, days: Int) -> Date
```

Adds (or subtracts, if negative) days from a date.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let d = time.date(2024, 1, 1)?
    println(d |> time.add_days(90))   -- 2024-03-31
    println(d |> time.add_days(-1))   -- 2023-12-31
    Ok(())
}
```


## `time.add_months`

```
time.add_months(date: Date, months: Int) -> Date
```

Adds (or subtracts) months from a date. Clamps to the last valid day of the target month.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let d = time.date(2024, 1, 31)?
    println(d |> time.add_months(1))   -- 2024-02-29 (leap year, clamped)
    println(d |> time.add_months(2))   -- 2024-03-31
    Ok(())
}
```


## `time.add`

```
time.add(instant: Instant, duration: Duration) -> Instant
```

Adds a duration to an instant.

```silt
import time
fn main() {
    let t = time.now()
    let later = t |> time.add(time.hours(2))
    println(time.since(t, later))  -- 2h
}
```


## `time.since`

```
time.since(from: Instant, to: Instant) -> Duration
```

Returns the signed duration from `from` to `to` (computed as `to.epoch_ns − from.epoch_ns`).

```silt
import time
fn main() {
    let start = time.now()
    time.sleep(time.ms(100))
    let elapsed = time.since(start, time.now())
    println(elapsed)  -- 100ms
}
```


## `time.hours`, `time.minutes`, `time.seconds`, `time.ms`, `time.micros`, `time.nanos`

```
time.hours(n: Int) -> Duration
time.minutes(n: Int) -> Duration
time.seconds(n: Int) -> Duration
time.ms(n: Int) -> Duration
time.micros(n: Int) -> Duration
time.nanos(n: Int) -> Duration
```

Duration constructor functions. All units return a `Duration` with
nanosecond precision; they differ only in the multiplier applied to
their `Int` argument. `time.nanos` is the raw form (no multiplication).
Overflowing the `Int` range (`i64::MAX` nanoseconds ≈ 292 years) is
surfaced as a runtime error rather than a silent wrap.

```silt
import time
fn main() {
    println(time.hours(1))      -- 1h
    println(time.minutes(30))   -- 30m
    println(time.seconds(5))    -- 5s
    println(time.ms(500))       -- 500ms
    println(time.micros(250))   -- 250us
    println(time.nanos(42))     -- 42ns
}
```


## `time.weekday`

```
time.weekday(date: Date) -> Weekday
```

Returns the day of the week. Pattern-match on the result for exhaustive handling.

```silt
import time
fn main() {
    let day = time.today() |> time.weekday
    match day {
        Monday -> println("start of the week")
        Friday -> println("almost weekend")
        Saturday | Sunday -> println("weekend!")
        _ -> println("midweek")
    }
}
```


## `time.days_between`

```
time.days_between(from: Date, to: Date) -> Int
```

Returns the signed number of days between two dates.

```silt
import time
fn main() -> Result(Unit, TimeError) {
    let a = time.date(2024, 1, 1)?
    let b = time.date(2024, 12, 31)?
    println(time.days_between(a, b))  -- 365
    Ok(())
}
```


## `time.days_in_month`

```
time.days_in_month(year: Int, month: Int) -> Int
```

Returns the number of days in the given month.

```silt
import time
fn main() {
    println(time.days_in_month(2024, 2))  -- 29 (leap year)
    println(time.days_in_month(2023, 2))  -- 28
}
```


## `time.is_leap_year`

```
time.is_leap_year(year: Int) -> Bool
```

Returns true if the year is a leap year.

```silt
import time
fn main() {
    println(time.is_leap_year(2024))  -- true
    println(time.is_leap_year(1900))  -- false (divisible by 100)
    println(time.is_leap_year(2000))  -- true (divisible by 400)
}
```


## `time.sleep`

```
time.sleep(duration: Duration) -> ()
```

Blocks the current task for the given duration. Other tasks continue running.

```silt
import time
fn main() {
    let before = time.now()
    time.sleep(time.ms(100))
    let elapsed = time.since(before, time.now())
    println(elapsed)  -- ~100ms
}
```
"#;

/// Verbatim former `docs/stdlib/toml.md`.
#[allow(dead_code)]
pub(super) const TOML_MD: &str = r#"---
title: "toml"
section: "Standard Library"
order: 11
---

# toml

Parse TOML documents into typed silt values and serialize values to TOML.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `parse` | `(String, type a) -> Result(a, TomlError)` | Parse a TOML document (top-level table) into a record |
| `parse_list` | `(String, type a) -> Result(List(a), TomlError)` | Parse a single `[[items]]` section into a list of records |
| `parse_map` | `(String, type v) -> Result(Map(String, v), TomlError)` | Parse a top-level table into a map |
| `pretty` | `(a) -> Result(String, TomlError)` | Pretty-print a value as TOML |
| `stringify` | `(a) -> Result(String, TomlError)` | Serialize a value as TOML |

## Errors

Every fallible `toml.*` call returns `Result(T, TomlError)`. The `TomlError`
enum has four variants you can pattern-match on, or fall back to
`e.message()` when you just want a rendered string (`trait Error for
TomlError` is wired in):

| Variant | Fields | Meaning |
|---------|--------|---------|
| `TomlSyntax(msg, offset)` | `String`, `Int` | Malformed TOML at `offset` bytes |
| `TomlTypeMismatch(expected, actual)` | `String`, `String` | A field's TOML type did not match the target field type |
| `TomlMissingField(name)` | `String` | A required (non-`Option`) field was absent |
| `TomlUnknown(msg)` | `String` | Anything else (serialization failures, document-shape violations) |

See [stdlib errors](errors.md) for the shared `Error` trait.


## `toml.parse`

```
toml.parse(s: String, type a) -> Result(a, TomlError)
```

Parses a TOML document into a record of type `a`. The type is passed as a
`type` parameter (see [Generics](../language/generics.md) — not a string).
Fields are matched by name; `Option` fields default to `None` if missing
from the document.

Fields of type `Date`, `Time`, and `DateTime` (from the `time` module) accept
either a TOML native datetime literal or an ISO 8601 string — the same formats
that `json.parse` accepts for each of those field types:

| Field type | Accepted formats | Example |
|------------|-----------------|---------|
| `Date` | `YYYY-MM-DD` | `1979-05-27` or `"1979-05-27"` |
| `Time` | `HH:MM:SS`, `HH:MM` | `07:32:00` or `"07:32:00"` |
| `DateTime` | RFC 3339 / ISO 8601, with optional `Z` or `±HH:MM` offset | `1979-05-27T07:32:00Z` |

```silt
import toml
type User {
    name: String,
    age: Int,
    active: Bool,
}

fn main() {
    let input = "name = \"Alice\"\nage = 30\nactive = true\n"
    match toml.parse(input, User) {
        Ok(user) -> println(user.name)
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `toml.parse_list`

```
toml.parse_list(s: String, type a) -> Result(List(a), TomlError)
```

TOML's spec requires the top level of every document to be a table, so there is
no direct equivalent of a JSON top-level array. `toml.parse_list` therefore
accepts a document whose top-level table contains exactly one key — an array-of-
tables — and returns the list of records under that key. This is the natural
shape of `[[items]]` sections:

```silt
import toml
import list
type Point { x: Int, y: Int }

fn main() {
    let input = "[[points]]\nx = 1\ny = 2\n\n[[points]]\nx = 3\ny = 4\n"
    match toml.parse_list(input, Point) {
        Ok(points) -> list.each(points) { p -> println("{p.x}, {p.y}") }
        Err(e) -> println("Error: {e.message()}")
    }
}
```

The key name (`points` in the example above) is not checked — any single
top-level array-of-tables key works.


## `toml.parse_map`

```
toml.parse_map(s: String, type v) -> Result(Map(String, v), TomlError)
```

Parses a top-level TOML table into a `Map(String, v)`. The type is passed as
a `type` parameter (`Int`, `Float`, `String`, `Bool`, or a record type).

```silt
import toml
import map
fn main() {
    let input = "x = 10\ny = 20\n"
    match toml.parse_map(input, Int) {
        Ok(m) -> println(map.get(m, "x"))  -- Some(10)
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `toml.pretty`

```
toml.pretty(value: a) -> Result(String, TomlError)
```

Serialises a silt record or map to a multi-line, human-friendly TOML document.
TOML's default output is already readable (one key per line, nested tables as
`[section]` headers), so `toml.pretty` differs only slightly from `toml.stringify`
— nested arrays of tables render as `[[section]]` blocks instead of inline
arrays.

Note: the top-level value must be a record or a map; TOML cannot represent a
bare scalar or array at top level.

```silt
import toml
fn main() {
    let data = #{"name": "silt", "version": "1.0"}
    match toml.pretty(data) {
        Ok(s) -> println(s)
        Err(e) -> println("Error: {e.message()}")
    }
}
```


## `toml.stringify`

```
toml.stringify(value: a) -> Result(String, TomlError)
```

Serialises a silt record or map to a TOML document. Like `toml.pretty`, the
top-level value must be table-shaped.

```silt
import toml
type Package { name: String, version: String }
fn main() {
    let data = Package { name: "silt", version: "1.0" }
    match toml.stringify(data) {
        Ok(s) -> println(s)
        Err(e) -> println("Error: {e.message()}")
    }
}
```
"#;

/// Verbatim former `docs/stdlib/uuid.md`.
#[allow(dead_code)]
pub(super) const UUID_MD: &str = r#"---
title: "uuid"
section: "Standard Library"
order: 19
---

# uuid

UUID generation, parsing, and validation. All UUIDs cross the language
boundary as `String` in the canonical lowercase hyphenated form:
`"550e8400-e29b-41d4-a716-446655440000"` (8-4-4-4-12, 36 characters).

Two generators are provided:

- `uuid.v4` — fully random UUIDs (RFC 9562 version 4). Random bits come
  from the OS CSPRNG via the [`getrandom`](https://crates.io/crates/getrandom)
  crate (the same source that backs `crypto.random_bytes`).
- `uuid.v7` — time-ordered UUIDs (RFC 9562 version 7, 2022 spec). The
  first 48 bits encode a Unix millisecond timestamp, the remainder is
  random. Lexicographic string comparison on two v7 UUIDs minted in
  order tracks generation time — ideal for B-tree-friendly primary
  keys where monotonic inserts avoid page splits, while still being
  unguessable.

Use `uuid.parse` when you need to validate and canonicalize a UUID
string at trust boundaries (e.g. parsing an HTTP path param). It
accepts any form the underlying parser understands (hyphenated,
simple/32-char, braced, urn-prefixed) and canonicalizes the output.
Use `uuid.is_valid` when you only need the boolean — no `Result`
allocation.

## Summary

| Function | Signature | Description |
|----------|-----------|-------------|
| `v4` | `() -> String` | Random UUID (version 4, CSPRNG-backed) |
| `v7` | `() -> String` | Time-ordered UUID (version 7, RFC 9562) |
| `parse` | `(String) -> Result(String, String)` | Validate + canonicalize any-version UUID |
| `nil` | `() -> String` | The all-zero UUID sentinel |
| `is_valid` | `(String) -> Bool` | Predicate form of `parse` |

## Examples

```silt
import uuid

fn main() {
  -- Random UUID
  let id = uuid.v4()
  println(id)
  -- e.g. 550e8400-e29b-41d4-a716-446655440000

  -- Time-ordered UUID (good for DB primary keys)
  let pk = uuid.v7()
  println(pk)

  -- Two v7s sort by generation time
  let a = uuid.v7()
  let b = uuid.v7()
  -- a < b when compared as strings

  -- Validate + canonicalize external input
  match uuid.parse("550E8400-E29B-41D4-A716-446655440000") {
    Ok(canonical) -> println(canonical)
    -- 550e8400-e29b-41d4-a716-446655440000
    Err(e) -> println(e)
  }

  -- Predicate form, no Result allocation
  match uuid.is_valid("not-a-uuid") {
    true -> println("valid")
    false -> println("invalid")
  }

  -- Sentinel
  println(uuid.nil())
  -- 00000000-0000-0000-0000-000000000000
}
```

## Errors

Only `parse` is fallible at the type level; the generators and `nil`
are total.

| Operation | Error condition |
|-----------|-----------------|
| `parse` | Input is not a syntactically valid UUID (`"invalid uuid: ..."`) |

## Notes

- Output is always the 36-character lowercase hyphenated form. If you
  need a different shape (braced, simple/32-char, uppercase), transform
  the returned string with `string` helpers — keeping one canonical
  wire format here avoids a combinatorial API surface.
- `uuid.parse` is version-agnostic: it accepts v1 through v8 UUIDs and
  the nil UUID. If you need to *reject* particular versions, match on
  the relevant substring of the returned canonical form; a dedicated
  `uuid.version` accessor can land later if demand warrants it.
- `uuid.v4` and `uuid.v7` collide with probability ~2^-122 and
  ~2^-74-per-ms respectively — treat collisions as impossible in
  practice, not as a case to handle at call sites.
- Prefer `uuid.v7` over `uuid.v4` for database primary keys: monotonic
  inserts keep B-tree pages hot and avoid the random-write amplification
  that v4 causes on sorted indexes.
"#;

// ── END AUTO-GENERATED MARKDOWN CONSTANTS ──
