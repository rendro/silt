//! What the crate promises an embedder (AR-8): the names docs/ffi.md, the
//! embedding guide, uses. `PROMISED` is that list: every name the
//! guide's Rust imports is on it, and every name on it is in the guide.
//! The rest of the crate's `pub` items are its own (the commands, the
//! language server, the playground and the tests use them) and may
//! change without notice; the test API is behind the `test-hooks`
//! feature.

use std::path::Path;

// Each promised name, imported here: the test does not build when one
// is gone or moved.
#[allow(unused_imports)]
use silt::session::{
    Builtins, Config, Entry, EntryPoint, HostCall, HostModule, LockPolicy, Program, ProjectSetup,
    Session, TestFn, TestKind,
};
#[allow(unused_imports)]
use silt::{Buffer, Clock, Exit, FromValue, HostIo, IntoValue, Output, Tag, Value, Vm, VmError};

/// The crate's promise to an embedder, by path.
const PROMISED: &[&str] = &[
    "silt::Buffer",
    "silt::Clock",
    "silt::Exit",
    "silt::FromValue",
    "silt::HostIo",
    "silt::IntoValue",
    "silt::Output",
    "silt::Tag",
    "silt::Value",
    "silt::Vm",
    "silt::VmError",
    "silt::session::Builtins",
    "silt::session::Config",
    "silt::session::Entry",
    "silt::session::EntryPoint",
    "silt::session::HostCall",
    "silt::session::HostModule",
    "silt::session::LockPolicy",
    "silt::session::Program",
    "silt::session::ProjectSetup",
    "silt::session::Session",
    "silt::session::TestFn",
    "silt::session::TestKind",
];

fn guide() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/ffi.md");
    std::fs::read_to_string(&path).expect("docs/ffi.md is read")
}

/// The paths a `use silt::...;` line or a `silt::a::B` in the text names:
/// `use silt::session::{Config, Session};` names two.
fn named_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("silt::") {
        let after = &rest[at..];
        let end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(after.len());
        let head = &after[..end];
        if after[end..].starts_with('{') {
            let close = after.find('}').expect("a `{` of an import closes");
            for item in after[end + 1..close].split(',') {
                let item = item.trim();
                if !item.is_empty() {
                    paths.push(format!("{head}{item}"));
                }
            }
            rest = &after[close..];
        } else {
            paths.push(head.trim_end_matches(':').to_string());
            rest = &after[end..];
        }
    }
    paths
}

#[test]
fn every_name_the_guide_imports_is_promised() {
    let named = named_paths(&guide());
    assert!(named.len() > 10, "{named:?}");
    let unpromised: Vec<&String> = (named.iter())
        .filter(|path| {
            !(PROMISED.iter()).any(|p| *path == p || path.starts_with(&format!("{p}::")))
        })
        .collect();
    assert!(
        unpromised.is_empty(),
        "docs/ffi.md uses names the crate does not promise (add each to PROMISED and to \
         the list in src/lib.rs, or take it out of the guide): {unpromised:?}"
    );
}

#[test]
fn every_promised_name_is_in_the_guide() {
    let guide = guide();
    let missing: Vec<&&str> = (PROMISED.iter())
        .filter(|path| {
            let name = path.rsplit("::").next().unwrap_or(path);
            !guide.contains(&format!("`{name}"))
                && !guide.contains(&format!("{name},"))
                && !guide.contains(&format!("{name}}}"))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "promised names the guide does not use: {missing:?}"
    );
}

/// The promise in src/lib.rs's crate documentation is this list.
#[test]
fn the_crate_documentation_lists_the_promise() {
    let lib = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .expect("src/lib.rs is read");
    let missing: Vec<&&str> = (PROMISED.iter())
        .filter(|path| !lib.contains(&format!("`{path}`")))
        .collect();
    assert!(missing.is_empty(), "src/lib.rs does not list: {missing:?}");
}

#[test]
fn the_names_of_an_import_are_read() {
    assert_eq!(
        named_paths("use silt::session::{Config, Session};\nlet x = silt::Value::Unit;"),
        [
            "silt::session::Config",
            "silt::session::Session",
            "silt::Value::Unit"
        ]
    );
}

// ── The surface: what outside code reaches ─────────────────────────────
//
// Two rules make `pub` mean "reachable from outside the crate": src/lib.rs
// makes only the roots below public, and every build with the test API
// warns of a `pub` that nothing outside reaches (`unreachable_pub`; CI's
// clippy runs with `-D warnings`). So the `pub` items of src are the
// whole surface: the promise and what it hands out, the command's entry,
// the playground's module and the test API. They are listed in
// tests/meta/public_items.txt: a `pub` item that is not on the list fails
// here, and so does one on the list that went. `SILT_BLESS=1` rewrites
// the list; read its diff, so that nothing is promised by accident.

use crate::env_switch;

/// The public lines of src/lib.rs outside the test API's module, each
/// with the attributes above it.
const ROOTS: &[&str] = &[
    "#[cfg(not(target_arch = \"wasm32\"))] #[doc(hidden)] pub mod cli;",
    "#[cfg(feature = \"playground\")] pub mod playground;",
    "pub mod session;",
    "pub use typeinfo::Tag;",
    "pub use value::{FromValue, IntoValue, Value};",
    "pub use vm::{Buffer, Clock, Exit, HostIo, Output, Vm, VmError};",
    "#[cfg(any(test, feature = \"test-hooks\"))] #[doc(hidden)] pub mod testing {",
];

fn source(path: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|e| panic!("{path} is read: {e}"))
}

#[test]
fn the_crate_root_makes_public_only_the_promise_the_command_the_playground_and_the_test_api() {
    let lib = source("src/lib.rs");
    let mut found = Vec::new();
    let mut attrs: Vec<&str> = Vec::new();
    for line in lib.lines() {
        if line.starts_with("#[") {
            attrs.push(line);
            continue;
        }
        if line.starts_with("pub ") {
            let mut with = attrs.join(" ");
            if !with.is_empty() {
                with.push(' ');
            }
            with.push_str(line);
            found.push(with);
        }
        if !line.starts_with("//") {
            attrs.clear();
        }
    }
    assert_eq!(
        found, ROOTS,
        "src/lib.rs makes another name public: a root is the promise, the command's \
         entry, the playground's module or the test API"
    );
}

/// What a `pub` line declares, without what follows its name: `pub fn
/// run`, `pub struct Session`, the field `pub name`, `pub use crate::x::`.
fn item_head(rest: &str) -> String {
    let bytes = rest.as_bytes();
    let mut end = rest.len();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b':' if bytes.get(i + 1) == Some(&b':') => i += 2,
            b'(' | b'<' | b'{' | b'=' | b';' | b',' | b':' => {
                end = i;
                break;
            }
            _ => i += 1,
        }
    }
    format!("pub {}", rest[..end].trim_end())
}

/// The items of the re-export `use a::b::{c, d as e};`: `pub use a::b::c`
/// and `pub use a::b::d as e`, so that a name added inside the braces
/// is an item of its own.
fn braces_items(text: &str) -> Vec<String> {
    let (path, rest) = text.split_once('{').expect("a re-export with braces");
    let path = path.trim_start_matches("use ").trim();
    let inside = rest.split('}').next().unwrap_or("");
    (inside.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("pub use {path}{name}"))
        .collect()
}

/// The `pub` items of `text`, one head each (see [`item_head`]). A
/// `pub(crate)`/`pub(super)` item is none, and text in strings and
/// comments is skipped.
fn pub_items(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_string = false;
    let mut raw_hashes: Option<usize> = None;
    let mut block_comments = 0usize;
    // A `pub use a::{b, c};` re-export, until its `}`: one item per name.
    let mut braces: Option<String> = None;
    for line in text.lines() {
        if let Some(open) = &mut braces {
            open.push_str(line);
            if line.contains('}') {
                items.extend(braces_items(open));
                braces = None;
            }
            continue;
        }
        if !in_string
            && raw_hashes.is_none()
            && block_comments == 0
            && let Some(rest) = line.trim_start().strip_prefix("pub ")
        {
            match rest.starts_with("use ") && rest.contains('{') {
                true if rest.contains('}') => items.extend(braces_items(rest)),
                true => braces = Some(rest.to_string()),
                false => items.push(item_head(rest)),
            }
        }
        // Where the line leaves the reader: in a string or a comment, or not.
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if let Some(hashes) = raw_hashes {
                let closes = bytes[i] == b'"'
                    && bytes.len() >= i + 1 + hashes
                    && bytes[i + 1..i + 1 + hashes].iter().all(|b| *b == b'#');
                if closes {
                    raw_hashes = None;
                    i += 1 + hashes;
                } else {
                    i += 1;
                }
            } else if in_string {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        in_string = false;
                        i += 1;
                    }
                    _ => i += 1,
                }
            } else if block_comments > 0 {
                if bytes[i..].starts_with(b"*/") {
                    block_comments -= 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"/*") {
                    block_comments += 1;
                    i += 2;
                } else {
                    i += 1;
                }
            } else if bytes[i..].starts_with(b"//") {
                break;
            } else if bytes[i..].starts_with(b"/*") {
                block_comments += 1;
                i += 2;
            } else if bytes[i] == b'\'' {
                // A char literal ('"', '\'', '\u{1b}') or a lifetime.
                match (bytes.get(i + 1), bytes.get(i + 2)) {
                    (Some(b'\\'), _) => {
                        let close = bytes[i + 2..].iter().skip(1).position(|b| *b == b'\'');
                        i += 4 + close.unwrap_or(0);
                    }
                    (Some(_), Some(b'\'')) => i += 3,
                    _ => i += 1,
                }
            } else if bytes[i] == b'r'
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
                && matches!(bytes.get(i + 1), Some(b'"') | Some(b'#'))
            {
                let hashes = bytes[i + 1..].iter().take_while(|b| **b == b'#').count();
                if bytes.get(i + 1 + hashes) == Some(&b'"') {
                    raw_hashes = Some(hashes);
                    i += 2 + hashes;
                } else {
                    i += 1;
                }
            } else if bytes[i] == b'"' {
                in_string = true;
                i += 1;
            } else {
                i += 1;
            }
        }
    }
    items
}

/// Every `pub` item of src, as `path: head`, sorted.
fn surface() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut dirs = vec![root.join("src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src is read") {
            let path = entry.expect("an entry of src").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let mut lines = Vec::new();
    for file in files {
        let name = (file.strip_prefix(root).expect("a file of the crate"))
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&file).expect("a source file is read");
        for item in pub_items(&text) {
            lines.push(format!("{name}: {item}"));
        }
    }
    lines.sort();
    lines
}

#[test]
fn every_public_item_is_on_the_list() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/meta/public_items.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let header: String = (text.lines())
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    let listed: Vec<String> = (text.lines())
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(str::to_string)
        .collect();
    let now = surface();
    if now != listed && env_switch::on("SILT_BLESS") {
        let mut text = header;
        for line in &now {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(&path, text).expect("tests/meta/public_items.txt is written");
        return;
    }
    let new: Vec<&String> = now.iter().filter(|l| !listed.contains(l)).collect();
    let gone: Vec<&String> = listed.iter().filter(|l| !now.contains(l)).collect();
    assert!(
        new.is_empty() && gone.is_empty() && now.len() == listed.len(),
        "the public items of src are not those of tests/meta/public_items.txt.\n\
         New (promise it, make it pub(crate), or put it in the test API or the playground's \
         module): {new:#?}\nGone: {gone:#?}\n\
         `SILT_BLESS=1 cargo nextest run --test meta public_api` rewrites the list."
    );
}

#[test]
fn the_reader_of_public_items_skips_strings_and_comments() {
    let text = "pub fn a(x: i32) {}\n    pub name: String,\nlet s = \"\npub fn not_me() {}\n\";\n\
                let r = r#\"\npub fn nor_me\n\"#;\n/* pub fn hidden\n*/\n// pub fn comment\n\
                let c = '\"';\npub(crate) fn inside() {}\npub use crate::x::{\n    a,\n    b,\n};\n\
                pub struct S<T>;\npub mod m;\npub(super) mod n;\n";
    assert_eq!(
        pub_items(text),
        [
            "pub fn a",
            "pub name",
            "pub use crate::x::a",
            "pub use crate::x::b",
            "pub struct S",
            "pub mod m"
        ]
    );
}

// ── The guide, as an embedder reads it ────────────────────────────────

/// The code spans of the guide's prose (outside its fenced blocks).
fn prose_code_spans(text: &str) -> Vec<String> {
    let mut prose = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            prose.push_str(line);
            prose.push('\n');
        }
    }
    let mut spans = Vec::new();
    let mut rest = prose.as_str();
    while let Some(at) = rest.find('`') {
        let ticks = rest[at..].chars().take_while(|c| *c == '`').count();
        let open = &rest[at..at + ticks];
        let after = &rest[at + ticks..];
        match after.find(open) {
            Some(end) => {
                spans.push(after[..end].trim().to_string());
                rest = &after[end + ticks..];
            }
            None => break,
        }
    }
    spans
}

/// The Rust paths a code span names by a type: `Tag::new` names `Tag`,
/// `vm.set_exit(Exit::EndsProcess)` names `Exit`. (A lowercase head,
/// `std::env` or `silt::session`, is a module: the paths that start with
/// `silt::` are checked with the guide's imports.)
fn type_heads(span: &str) -> Vec<String> {
    let mut heads = Vec::new();
    let bytes = span.as_bytes();
    let mut i = 0;
    while let Some(at) = span[i..].find("::") {
        let end = i + at;
        let start = span[..end]
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(0, |p| p + 1);
        let head = &span[start..end];
        let qualified = start >= 2 && &bytes[start - 2..start] == b"::";
        if !qualified && head.starts_with(|c: char| c.is_ascii_uppercase()) {
            heads.push(head.to_string());
        }
        i = end + 2;
    }
    heads
}

#[test]
fn every_type_the_guide_s_prose_names_is_promised() {
    let promised: Vec<&str> = (PROMISED.iter())
        .map(|path| path.rsplit("::").next().unwrap_or(path))
        .collect();
    let spans = prose_code_spans(&guide());
    assert!(spans.len() > 100, "{} spans", spans.len());
    let unpromised: Vec<(String, String)> = (spans.iter())
        .flat_map(|span| type_heads(span).into_iter().map(move |h| (h, span.clone())))
        .filter(|(head, _)| !promised.contains(&head.as_str()))
        .collect();
    assert!(
        unpromised.is_empty(),
        "docs/ffi.md's prose names types the crate does not promise (promise each, in \
         PROMISED, src/lib.rs's list and a re-export at its path, or reword the guide): \
         {unpromised:?}"
    );
}

#[test]
fn the_heads_of_a_code_span_are_read() {
    assert_eq!(type_heads("vm.set_exit(Exit::EndsProcess)"), ["Exit"]);
    assert_eq!(type_heads("HostIo::buffer(&buffer)"), ["HostIo"]);
    assert_eq!(type_heads("std::panic::catch_unwind"), Vec::<String>::new());
    assert_eq!(
        type_heads("silt::session::Session::new"),
        Vec::<String>::new()
    );
    assert_eq!(
        prose_code_spans("a `Tag::new` b\n```rust\nlet x = `y`;\n```\n``a `b` c``"),
        ["Tag::new", "a `b` c"]
    );
}

/// The guide's Rust blocks are held, each with what it needs around it,
/// in tests/lang/ffi_doc_blocks_tests.rs (whose own test fails when the
/// guide has a block it does not hold). Built there, they see the test
/// API, which the crate's tests always have. Here that file is checked
/// as the code of a crate of its own that depends on silt as an embedder
/// does: by path, with the default features and no `test-hooks`. A name
/// a block uses that is not promised does not resolve.
#[test]
fn the_guide_s_blocks_build_against_silt_as_an_embedder_sees_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("target").join("guide-as-an-embedder");
    std::fs::create_dir_all(dir.join("src")).expect("the crate's directory is made");
    let manifest = format!(
        "[package]\nname = \"silt-guide-as-an-embedder\"\nversion = \"0.0.0\"\n\
         edition = \"2024\"\npublish = false\n\n[dependencies]\nsilt = {{ path = '{}' }}\n\n\
         [workspace]\n",
        root.display()
    );
    std::fs::write(dir.join("Cargo.toml"), manifest).expect("Cargo.toml is written");
    // The versions of silt's dependencies are the root's.
    std::fs::copy(root.join("Cargo.lock"), dir.join("Cargo.lock")).expect("the lock is copied");
    std::fs::write(
        dir.join("src/lib.rs"),
        source("tests/lang/ffi_doc_blocks_tests.rs"),
    )
    .expect("the blocks are written");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["check", "--tests", "--offline", "--quiet", "-j", "2"])
        .arg("--manifest-path")
        .arg(dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("RUSTFLAGS")
        .output()
        .expect("cargo runs");
    assert!(
        out.status.success(),
        "the guide's blocks do not build for an embedder (silt with its default features, \
         no test-hooks):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The members of the promised types, read from src: each method and
/// associated function of an inherent impl (`impl Session {`), each
/// field of the type's struct (`pub struct Config {`) and each method of
/// a promised trait (`pub trait Clock {`), with whether it is public.
fn promised_members() -> Vec<(String, String, bool)> {
    let promised: Vec<&str> = (PROMISED.iter())
        .map(|path| path.rsplit("::").next().unwrap_or(path))
        .collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut dirs = vec![root.join("src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src is read") {
            let path = entry.expect("an entry of src").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let mut members = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("a source file is read");
        let mut depth = 0i32;
        // (the depth its block opened at, the type, whether a trait's)
        let mut open: Option<(i32, String, bool)> = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if depth == 0 && open.is_none() {
                let head = |rest: &str| -> String {
                    rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or("")
                        .to_string()
                };
                let found = if let Some(rest) = trimmed.strip_prefix("impl") {
                    // `impl X {` or `impl<'a> X<'a> {`, not `impl T for X {`.
                    let rest = match rest.starts_with('<') {
                        true => rest.split_once('>').map_or("", |(_, r)| r),
                        false => rest,
                    };
                    let rest = rest.trim_start();
                    (!trimmed.contains(" for ") && trimmed.ends_with('{'))
                        .then(|| (head(rest), false))
                } else if let Some(rest) = trimmed
                    .strip_prefix("pub struct ")
                    .or_else(|| trimmed.strip_prefix("pub(crate) struct "))
                {
                    trimmed.ends_with('{').then(|| (head(rest), false))
                } else if let Some(rest) = trimmed.strip_prefix("pub trait ") {
                    trimmed.ends_with('{').then(|| (head(rest), true))
                } else {
                    None
                };
                if let Some((ty, is_trait)) = found
                    && promised.contains(&ty.as_str())
                {
                    open = Some((depth, ty, is_trait));
                }
            } else if let Some((at, ty, is_trait)) = &open
                && depth == at + 1
            {
                let public = trimmed.starts_with("pub ");
                let rest = (trimmed.strip_prefix("pub(crate) "))
                    .or_else(|| trimmed.strip_prefix("pub(super) "))
                    .or_else(|| trimmed.strip_prefix("pub "))
                    .unwrap_or(trimmed);
                let rest = rest.strip_prefix("const ").unwrap_or(rest);
                let name = match rest.strip_prefix("fn ") {
                    Some(f) => f.split(['(', '<']).next(),
                    None if !rest.starts_with("//") && !rest.starts_with('#') => {
                        rest.split_once(':').map(|(n, _)| n)
                    }
                    None => None,
                };
                if let Some(name) = name.map(str::trim)
                    && !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    members.push((ty.clone(), name.to_string(), public || *is_trait));
                }
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if open.as_ref().is_some_and(|(at, _, _)| depth <= *at) {
                open = None;
            }
        }
    }
    members
}

/// The receivers the guide writes for a value of a promised type, with
/// the type: `vm.settle()` is Vm's `settle`.
const RECEIVERS: &[(&str, &str)] = &[
    ("vm", "Vm"),
    ("session", "Session"),
    ("call", "HostCall"),
    ("program", "Program"),
];

/// [`member_uses`] without the types.
fn member_names(span: &str) -> Vec<String> {
    member_uses(span)
        .into_iter()
        .map(|(_, name)| name)
        .collect()
}

/// The members a code span of the guide uses, each with the type the
/// span names it on where it says one (`Tag::new`, or a receiver of
/// [`RECEIVERS`]: `vm.set_exit(..)` is Vm's `set_exit`): a bare name
/// (`take()`, `out_of_steps`), each name after a `.`, and each name after
/// `Type::`. The names of silt itself are none: a span with a space (an
/// output, a line of a program), a builtin module or function (`list`,
/// `print`), and what follows a builtin module (`task.deadline`).
fn member_uses(span: &str) -> Vec<(Option<String>, String)> {
    let modules = silt::testing::module::builtin_modules();
    let functions = silt::testing::module::builtin_free_function_names();
    // A span with a space (a line of a program, an output) names no bare
    // member, but each name after a `.` or a `Type::` outside its quotes.
    let unquoted: String;
    let span = if span.contains(char::is_whitespace) {
        let mut kept = String::new();
        let mut quote: Option<char> = None;
        for c in span.chars() {
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '\'' || c == '"' => quote = Some(c),
                None => kept.push(c),
            }
        }
        unquoted = kept;
        unquoted.as_str()
    } else {
        span
    };
    let spaced = span.contains(char::is_whitespace);
    let ident = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect()
    };
    let bare = span.trim_end_matches("()");
    if !spaced && (modules.contains(&bare) || functions.contains(&bare)) {
        return Vec::new();
    }
    if !spaced
        && !bare.is_empty()
        && bare
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return vec![(None, bare.to_string())];
    }
    let mut names = Vec::new();
    for (at, _) in span.match_indices('.') {
        let name = ident(&span[at + 1..]);
        let receiver = span[..at]
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        if name.starts_with(|c: char| c.is_ascii_lowercase()) && !modules.contains(&receiver) {
            let ty = (RECEIVERS.iter())
                .find(|(written, _)| *written == receiver)
                .map(|(_, ty)| ty.to_string());
            names.push((ty, name));
        }
    }
    for (at, _) in span.match_indices("::") {
        let name = ident(&span[at + 2..]);
        let head = span[..at]
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .filter(|h| h.starts_with(|c: char| c.is_ascii_uppercase()));
        if let Some(head) = head
            && name.starts_with(|c: char| c.is_ascii_lowercase())
        {
            names.push((Some(head.to_string()), name));
        }
    }
    names
}

/// The uses of `uses` (a type where the span names one, the member's
/// name, the span) that name a member of a promised type that is not
/// public: on the type the span names, where it is one of `members`
/// that has a member of that name; else where no promised type has a
/// public member of that name. (A name no promised type has a member
/// of, `main` or `print`, is not the crate's API.)
fn hidden_members(
    uses: &[(Option<String>, String, String)],
    members: &[(String, String, bool)],
) -> Vec<(String, String)> {
    (uses.iter())
        .filter(|(ty, name, _)| {
            let of_name: Vec<_> = members.iter().filter(|(_, n, _)| n == name).collect();
            let on_type = |t: &String| -> Vec<_> {
                of_name.iter().filter(|(owner, _, _)| owner == t).collect()
            };
            match ty.as_ref().map(on_type).filter(|on| !on.is_empty()) {
                Some(on) => !on.iter().any(|(_, _, public)| *public),
                None => !of_name.is_empty() && !of_name.iter().any(|(_, _, public)| *public),
            }
        })
        .map(|(_, name, span)| (name.clone(), span.clone()))
        .collect()
}

/// A member of a promised type that the guide names in a code span, in
/// any form, is public: the guide does not tell an embedder to use what
/// the crate keeps to itself. Where the span says the type (`Session::`,
/// `session.`), the member of that type ([`hidden_members`]).
#[test]
fn every_member_the_guide_names_is_public() {
    let members = promised_members();
    assert!(members.len() > 50, "{} members read", members.len());
    let uses: Vec<(Option<String>, String, String)> = (prose_code_spans(&guide()).iter())
        .flat_map(|span| {
            member_uses(span)
                .into_iter()
                .map(move |(ty, name)| (ty, name, span.clone()))
        })
        .collect();
    let hidden = hidden_members(&uses, &members);
    assert!(
        hidden.is_empty(),
        "docs/ffi.md tells an embedder to use members of promised types that are not public \
         (make each public, or take it out of the guide): {hidden:?}"
    );
}

#[test]
fn the_member_names_of_a_code_span_are_read() {
    assert_eq!(member_names("take()"), ["take"]);
    assert_eq!(member_names("out_of_steps"), ["out_of_steps"]);
    assert_eq!(member_names("vm.set_exit(Exit::EndsProcess)"), ["set_exit"]);
    assert_eq!(
        member_names("failure.note(session.sources())"),
        ["note", "sources"]
    );
    assert_eq!(member_names("Tag::new"), ["new"]);
    assert_eq!(
        member_names("std::panic::catch_unwind"),
        Vec::<String>::new()
    );
    assert_eq!(member_names("task.deadline"), Vec::<String>::new());
    assert_eq!(member_names("print"), Vec::<String>::new());
    assert_eq!(member_names("'mylib.f' panicked"), Vec::<String>::new());
    assert_eq!(member_names("session.graph(a, b)"), ["graph"]);
    assert_eq!(member_names("let x = vm.settle()"), ["settle"]);
}

/// The type a span names a member on is the one asked: a name hidden on
/// it is hidden, though another promised type has a public member of the
/// name. FAILS on the base: any promised type's public member of the
/// name passed (`Session::name` for Tag's).
#[test]
fn a_member_is_asked_of_the_type_the_span_names() {
    let members: Vec<(String, String, bool)> = [
        ("Session", "name", false),
        ("Tag", "name", true),
        ("Vm", "exit", false),
        ("VmError", "exit", true),
        ("Session", "entry", false),
        ("Program", "entry", true),
        ("Session", "sources", true),
    ]
    .iter()
    .map(|(t, n, p)| (t.to_string(), n.to_string(), *p))
    .collect();
    let hidden = |span: &str| {
        let uses: Vec<_> = (member_uses(span).into_iter())
            .map(|(ty, name)| (ty, name, span.to_string()))
            .collect();
        hidden_members(&uses, &members)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
    };
    assert_eq!(hidden("Session::name"), ["name"]);
    assert_eq!(hidden("vm.exit"), ["exit"]);
    assert_eq!(hidden("session.entry"), ["entry"]);
    assert_eq!(hidden("let p = program.entry"), Vec::<String>::new());
    assert_eq!(hidden("Tag::name"), Vec::<String>::new());
    assert_eq!(
        hidden("failure.note(session.sources())"),
        Vec::<String>::new()
    );
    // A receiver the guide does not tie to a type: any public one does.
    assert_eq!(hidden("e.exit"), Vec::<String>::new());
    assert_eq!(hidden("name"), Vec::<String>::new());
}
