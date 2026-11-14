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
    Builtins, Config, Entry, HostCall, HostModule, LockPolicy, Program, ProjectSetup, Session,
};
#[allow(unused_imports)]
use silt::{Buffer, Clock, Exit, FromValue, HostIo, IntoValue, Output, Value, Vm, VmError};

/// The crate's promise to an embedder, by path.
const PROMISED: &[&str] = &[
    "silt::Buffer",
    "silt::Clock",
    "silt::Exit",
    "silt::FromValue",
    "silt::HostIo",
    "silt::IntoValue",
    "silt::Output",
    "silt::Value",
    "silt::Vm",
    "silt::VmError",
    "silt::session::Builtins",
    "silt::session::Config",
    "silt::session::Entry",
    "silt::session::HostCall",
    "silt::session::HostModule",
    "silt::session::LockPolicy",
    "silt::session::Program",
    "silt::session::ProjectSetup",
    "silt::session::Session",
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

use super::stdlib_reference_tests::env_switch;

/// The public lines of src/lib.rs outside the test API's module, each
/// with the attributes above it.
const ROOTS: &[&str] = &[
    "#[cfg(not(target_arch = \"wasm32\"))] #[doc(hidden)] pub mod cli;",
    "#[cfg(feature = \"playground\")] pub mod playground;",
    "pub mod session;",
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

/// The `pub` items of `text`, one head each (see [`item_head`]). A
/// `pub(crate)`/`pub(super)` item is none, and text in strings and
/// comments is skipped.
fn pub_items(text: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut in_string = false;
    let mut raw_hashes: Option<usize> = None;
    let mut block_comments = 0usize;
    for line in text.lines() {
        if !in_string
            && raw_hashes.is_none()
            && block_comments == 0
            && let Some(rest) = line.trim_start().strip_prefix("pub ")
        {
            items.push(item_head(rest));
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
                let c = '\"';\npub(crate) fn inside() {}\npub use crate::x::{\n\
                pub struct S<T>;\npub mod m;\npub(super) mod n;\n";
    assert_eq!(
        pub_items(text),
        [
            "pub fn a",
            "pub name",
            "pub use crate::x::",
            "pub struct S",
            "pub mod m"
        ]
    );
}
