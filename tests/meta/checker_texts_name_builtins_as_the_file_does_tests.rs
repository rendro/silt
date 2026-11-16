//! The checker writes a builtin module's function, type or trait in a
//! message or a help as the file at hand writes it (audit R9, card NAM-1):
//! through the naming function (`TypeChecker::written_builtin`,
//! `Session::written_builtin`) when the text is built, never as a name
//! written into the text itself. This lock reads the string literals of the
//! checker's sources and of the places that build its helps, and fails on
//! one that writes `module.name` of a builtin module inside its text
//! (`"use `result.map_err`"`). A literal that is the name alone
//! (`"result.map_err"`, what is given to the naming function or looked up
//! in the registry) is no text. A text that must keep the module's own
//! name (code for another module, a builtin trait's docs) says so in a
//! comment ending in "own name)" on one of the four lines above it.

use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The files read: every source of the checker but its tests, the
/// session's entry-point helps and the decoders' help texts.
fn files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![root().join("src/typechecker")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("src/typechecker is read") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "tests") {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path
                    .file_name()
                    .is_some_and(|n| n == "tests.rs" || n == "test_helpers.rs")
            {
                files.push(path);
            }
        }
    }
    files.push(root().join("src/session/entry.rs"));
    files.push(root().join("src/builtins/registry/decoding.rs"));
    files.sort();
    files
}

/// The string literals of `text` outside comments, each with the byte it
/// starts at: `"..."` with its escapes, `r"..."` and `r#"..."#`. Character
/// literals are passed over.
fn literals(text: &str) -> Vec<(usize, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with("//") {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if text[i..].starts_with("/*") {
            i = text[i..].find("*/").map_or(bytes.len(), |n| i + n + 2);
            continue;
        }
        // A character literal: 'x', '\n', '"'.
        if bytes[i] == b'\'' {
            let quote_after = |k: usize| bytes.get(i + k) == Some(&b'\'');
            if quote_after(2) {
                i += 3;
                continue;
            }
            if bytes.get(i + 1) == Some(&b'\\') && quote_after(3) {
                i += 4;
                continue;
            }
        }
        let ident_before = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if bytes[i] == b'r' && !ident_before {
            let hashes = text[i + 1..].bytes().take_while(|b| *b == b'#').count();
            if bytes.get(i + 1 + hashes) == Some(&b'"') {
                let start = i + 2 + hashes;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = text[start..]
                    .find(&close)
                    .map_or(bytes.len(), |n| start + n);
                out.push((start, text[start..end].to_string()));
                i = end + close.len();
                continue;
            }
        }
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            out.push((i + 1, text[i + 1..j.min(bytes.len())].to_string()));
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// The builtin module's `name.member` that `literal` writes into its
/// text, if any: a name of a builtin module followed by a dot and a
/// letter, not part of a longer name. None for a literal that is such a
/// name alone.
fn written_name(literal: &str, modules: &[&str]) -> Option<String> {
    let is_name = |s: &str| {
        s.split_once('.').is_some_and(|(module, member)| {
            modules.contains(&module)
                && !member.is_empty()
                && member.chars().all(|c| c.is_alphanumeric() || c == '_')
        })
    };
    if is_name(literal) {
        return None;
    }
    for (at, _) in literal.char_indices() {
        let before = literal[..at].chars().last();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':') {
            continue;
        }
        let rest = &literal[at..];
        let Some(dot) = rest.find('.') else { continue };
        let module = &rest[..dot];
        let after = rest[dot + 1..].chars().next();
        if modules.contains(&module) && after.is_some_and(|c| c.is_alphabetic() || c == '_') {
            let member: String = rest[dot + 1..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            return Some(format!("{module}.{member}"));
        }
    }
    None
}

/// `text` without a `#[cfg(test)] mod tests` at its end.
fn without_test_module(text: &str) -> &str {
    match text.find("#[cfg(test)]\nmod tests") {
        Some(at) => &text[..at],
        None => text,
    }
}

#[test]
fn no_checker_text_writes_a_builtin_module_s_name_itself() {
    let modules = silt::testing::module::builtin_modules();
    let mut found = Vec::new();
    for file in files() {
        let source = std::fs::read_to_string(&file).expect("a source file is read");
        let text = without_test_module(&source);
        for (start, literal) in literals(text) {
            let Some(name) = written_name(&literal, modules) else {
                continue;
            };
            let lines: Vec<&str> = text[..start].lines().collect();
            let marked = lines
                .iter()
                .rev()
                .take(4)
                .any(|line| line.contains("own name)"));
            if !marked {
                let line = text[..start].lines().count();
                let path = file
                    .strip_prefix(root())
                    .unwrap_or(&file)
                    .display()
                    .to_string();
                found.push(format!("{}:{line}: `{name}`", path.replace('\\', "/")));
            }
        }
    }
    assert!(
        found.is_empty(),
        "checker texts write a builtin's name themselves; build it with written_builtin (or, \
         for a text that keeps the module's own name, say why in a comment ending in \
         \"own name)\"):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_reader_finds_a_written_name_and_passes_over_a_name_alone() {
    let modules = ["result", "json", "list"];
    assert_eq!(
        written_name("to change the error, use `result.map_err`", &modules).as_deref(),
        Some("result.map_err")
    );
    assert_eq!(written_name("result.map_err", &modules), None);
    assert_eq!(written_name("a `{}` impl", &modules), None);
    assert_eq!(written_name("my_list.len and xjson.Value", &modules), None);
    assert_eq!(
        written_name("trait 'json.Decode'", &modules).as_deref(),
        Some("json.Decode")
    );
    let text = "let a = \"use `list.map`\"; // \"list.find\" in a comment\nlet c = '\"';";
    let found: Vec<String> = literals(text).into_iter().map(|(_, l)| l).collect();
    assert_eq!(found, ["use `list.map`"]);
}
