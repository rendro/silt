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
