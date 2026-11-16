//! The checker writes a builtin module's function, type or trait in a
//! message or a help as the file at hand writes it (audit R9, card NAM-1):
//! through the naming function (`TypeChecker::written_builtin`,
//! `Session::written_builtin`, `written` where that is passed on) when the
//! text is built. This test is a tripwire for the plain slips, not a proof.
//! It reads the string literals of the checker (src/typechecker), of the
//! session and the compiler (src/session, src/compiler), all but their
//! tests, and of the decoders' help texts (src/builtins/registry/
//! decoding.rs), and fails on
//!
//! - a literal that writes a builtin's `module.name` into its text
//!   (`"use `result.map_err`"`);
//! - a literal that is a builtin's `module.name` alone (`"result.map_err"`)
//!   and is not handed straight to the naming function (`written_builtin(`
//!   or `written(` right before it): the realistic way round the first
//!   rule, `format!("use `{}`", "result.map_err")`.
//!
//! It does not see a name put together at run time: the module as an
//! argument (`format!("`{}.map_err`", "result")`), `concat!`, a `const`, an
//! escape (`result\u{2e}map_err`); nor the files it does not read (the
//! command's and the REPL's help screens, the editor's own texts, the
//! parser's helps, which are written before any import is known). A
//! literal that must keep the module's own name (code for another module,
//! a builtin trait's docs) or is no text at all (a registry key, a leaf's
//! name) says so in a comment ending in "own name)" or "not a text)" on
//! its own line or the line above it; a mark excuses nothing further down.
//! Text behind a file's `#[cfg(test)] mod tests` is not read (the tests).

use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The files read: every source of the checker, the session and the
/// compiler but their tests, and the decoders' help texts.
fn files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs: Vec<PathBuf> = ["src/typechecker", "src/session", "src/compiler"]
        .iter()
        .map(|dir| root().join(dir))
        .collect();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("a source directory is read") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "tests") {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path
                    .file_name()
                    .is_some_and(|n| n == "tests.rs" || n == "test_helpers.rs" || n == "testing.rs")
            {
                files.push(path);
            }
        }
    }
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

/// Whether `literal` is a builtin's `module.name` alone.
fn is_name(literal: &str, modules: &[&str]) -> bool {
    literal.split_once('.').is_some_and(|(module, member)| {
        modules.contains(&module)
            && !member.is_empty()
            && member.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Whether the source before a literal (`before`, up to its opening
/// quote) hands it straight to the naming function: a call of
/// `written_builtin(` or `written(` (the name passed on as a parameter),
/// the whole name (`unwritten(` is not it).
fn given_to_the_naming_function(before: &str) -> bool {
    let before = before.trim_end();
    let before = before.strip_suffix('&').unwrap_or(before).trim_end();
    ["written_builtin(", "written("].iter().any(|call| {
        before.strip_suffix(call).is_some_and(|head| {
            !head
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

/// The builtin module's `name.member` that `literal` writes into its
/// text, if any: a name of a builtin module followed by a dot and a
/// letter, not part of a longer name. None for a literal that is such a
/// name alone.
fn written_name(literal: &str, modules: &[&str]) -> Option<String> {
    if is_name(literal, modules) {
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
            // (The opening quote is the byte before `start`, or `r#"` for a
            // raw literal: what stands before it is the call it is given to.)
            let quote = text[..start].rfind(['"']).unwrap_or(0);
            let name = match written_name(&literal, modules) {
                Some(name) => format!("`{name}` in a text"),
                None if is_name(&literal, modules)
                    && !given_to_the_naming_function(&text[..quote]) =>
                {
                    format!("`{literal}` alone, not given to the naming function")
                }
                None => continue,
            };
            // (Its own line, then the one above it; a literal's own line
            // runs on to the line's end.)
            let line_end = text[start..].find('\n').map_or(text.len(), |n| start + n);
            let lines: Vec<&str> = text[..line_end].lines().collect();
            let marked = lines
                .iter()
                .rev()
                .take(2)
                .any(|line| line.contains("own name)") || line.contains("not a text)"));
            if !marked {
                let line = text[..start].lines().count();
                let path = file
                    .strip_prefix(root())
                    .unwrap_or(&file)
                    .display()
                    .to_string();
                found.push(format!("{}:{line}: {name}", path.replace('\\', "/")));
            }
        }
    }
    assert!(
        found.is_empty(),
        "checker texts write a builtin's name themselves; build it with written_builtin (or, \
         for a text that keeps the module's own name or a literal that is no text, say why in \
         a comment ending in \"own name)\" or \"not a text)\"):\n{}",
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
    assert!(given_to_the_naming_function("x = self.written_builtin("));
    assert!(given_to_the_naming_function(
        "f(&self.written_builtin(\n    "
    ));
    assert!(given_to_the_naming_function("written("));
    assert!(!given_to_the_naming_function("format!(\"use `{}`\", "));
    assert!(!given_to_the_naming_function("unwritten("));
    assert!(!given_to_the_naming_function("self.not_written("));
    let text = "let a = \"use `list.map`\"; // \"list.find\" in a comment\nlet c = '\"';";
    let found: Vec<String> = literals(text).into_iter().map(|(_, l)| l).collect();
    assert_eq!(found, ["use `list.map`"]);
}
