//! The rename sweep: at every identifier of a program, ask the server to
//! rename it, apply the edits to a copy, and require that the copy is
//! the same program under another name, or that the rename was refused.
//!
//! "The same program": `silt check` of every file of the case says the
//! same, `silt disasm` of the entry is the same, and (for a case, not
//! for the examples) `silt run` prints the same, each with the old and
//! the new name read as one name. A rename that edits text other than
//! the old name, or that does not edit the place it was asked at, fails
//! too; so does one that `prepareRename` refused.
//!
//! Each identifier is asked with a fresh name of its kind (`qq7`,
//! `Qq7`) and, in a case, with names the file already uses and names of
//! silt's own: a rename that would capture, or be captured, has to be
//! refused. The cases are in `tests/lsp/rename_sweep/` and are swept by
//! `rename_sweep_tests`; the examples are swept with the fresh name by
//! the heavy suite's `rename_sweep_examples`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::support::LspClient;

pub(crate) fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A `file://` URI for `path`, as the other LSP tests write them.
fn uri_of(path: &Path) -> String {
    silt::lsp::path_to_file_uri(path)
        .map(|uri| uri.as_str().to_string())
        .expect("a file URI")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Every `.silt` file under `dir`, relative to it, sorted.
pub(crate) fn silt_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "silt") {
                out.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// What `silt <args>` says in `dir`: status, stdout, stderr.
fn silt(dir: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_silt"))
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .env_remove("FORCE_COLOR")
        .output()
        .expect("run silt");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `text` with every identifier `a` or `b` written as one placeholder:
/// two programs that differ in that name only read the same.
fn same_name(text: &str, a: &str, b: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word == a || word == b {
            out.push_str("\u{1}NAME\u{1}");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// What is compared: the check of every file, the disasm of the entry,
/// and the run of the entry when `run`.
fn verdict(dir: &Path, files: &[PathBuf], entry: &str, run: bool) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        let name = file.to_string_lossy().replace('\\', "/");
        let (code, stdout, stderr) = silt(dir, &["check", &name]);
        out.push(format!("check {name}: {code:?}\n{stdout}{stderr}"));
    }
    let (code, stdout, stderr) = silt(dir, &["disasm", entry]);
    out.push(format!("disasm: {code:?}\n{stdout}{stderr}"));
    if run {
        let (code, stdout, stderr) = silt(dir, &["run", entry]);
        out.push(format!("run: {code:?}\n{stdout}{stderr}"));
    }
    out
}

/// The identifiers of `text`: 0-based line, UTF-16 column, and the word.
fn identifiers(text: &str) -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    for (line_no, line) in text.split('\n').enumerate() {
        let mut col = 0u32;
        let mut word: Option<(u32, String)> = None;
        for c in line.chars().chain(std::iter::once(' ')) {
            if c.is_ascii_alphanumeric() || c == '_' {
                word.get_or_insert((col, String::new())).1.push(c);
            } else if let Some((start, w)) = word.take()
                && !w.starts_with(|c: char| c.is_ascii_digit())
            {
                out.push((line_no as u32, start, w));
            }
            col += c.len_utf16() as u32;
        }
    }
    out
}

/// The byte offset of the UTF-16 position `(line, character)` of `text`.
fn offset_of(text: &str, line: u64, character: u64) -> usize {
    let mut offset = 0;
    for (n, l) in text.split('\n').enumerate() {
        if n as u64 == line {
            let mut units = 0;
            for (at, c) in l.char_indices() {
                if units >= character {
                    return offset + at;
                }
                units += c.len_utf16() as u64;
            }
            return offset + l.len();
        }
        offset += l.len() + 1;
    }
    text.len()
}

#[derive(Default)]
pub(crate) struct Outcome {
    /// Rename requests sent.
    pub(crate) asked: usize,
    /// Distinct renames that were applied and compared.
    pub(crate) renamed: usize,
    /// Requests the server refused (an error or no edit).
    pub(crate) refused: usize,
    pub(crate) broken: Vec<String>,
}

/// Sweep the file `entry` of the case directory `case`. `run`: also
/// compare what `silt run <run_entry>` prints. `clash`: also ask with
/// names in use. `every`: verify every n-th distinct rename (1 = all).
pub(crate) fn sweep(
    case: &Path,
    entry: &str,
    run_entry: &str,
    run: bool,
    clash: bool,
    every: usize,
) -> Outcome {
    let scratch = std::env::temp_dir().join("silt_rename_sweep").join(format!(
        "{}_{}_{}",
        case.file_name().unwrap().to_string_lossy(),
        entry.replace(['/', '.'], "_"),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let root = scratch.join("c");
    copy_dir(case, &root);
    let root = silt::source::canonical_path(&root);
    // A package's lockfile is written once, by the CLI: the server only
    // reads it.
    silt(&root, &["check", run_entry]);
    // The files whose check is compared: every file of a case; of the
    // examples, which do not import each other, the entry.
    let files = match run {
        true => silt_files(&root),
        false => vec![PathBuf::from(entry)],
    };
    let base = verdict(&root, &files, run_entry, run);
    // A case is a program that runs, an example one that checks: a
    // rename of a broken program would be compared with its errors.
    let (what, sound) = match run {
        true => ("run", base.last().unwrap().starts_with("run: Some(0)\n")),
        false => ("check", base[0].contains(".silt: Some(0)\n")),
    };
    assert!(
        sound,
        "{}: {run_entry} does not {what}:\n{}",
        case.display(),
        base.join("\n")
    );

    let all_files = silt_files(&root);
    let text = std::fs::read_to_string(root.join(entry)).unwrap();
    let entry_uri = uri_of(&root.join(entry));
    let mut client = LspClient::spawn_with_root(Some(&uri_of(&root)));
    client.did_open_and_wait(&entry_uri, &text);

    let words = identifiers(&text);
    // Names in use, by kind: the most frequent of the file, and silt's own.
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, _, w) in &words {
        *counts.entry(w.as_str()).or_default() += 1;
    }
    let mut by_count: Vec<(&str, usize)> = counts.into_iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let upper = |name: &str| name.starts_with(|c: char| c.is_ascii_uppercase());
    let in_use = |want_upper: bool| -> Vec<String> {
        let mut names: Vec<String> = by_count
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| upper(name) == want_upper)
            .filter(|name| !silt::lexer::KEYWORDS.contains(name))
            .take(3)
            .map(str::to_string)
            .collect();
        names.push(if want_upper { "Some" } else { "println" }.to_string());
        names
    };
    let (clash_lower, clash_upper) = (in_use(false), in_use(true));

    let mut outcome = Outcome::default();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut distinct = 0usize;
    for (line, col, name) in &words {
        // The entry function is found by its name: under another name
        // the program has no entry, and is another program.
        if name == "main" {
            continue;
        }
        let mut names = vec![if upper(name) { "Qq7" } else { "qq7" }.to_string()];
        if clash {
            names.extend(if upper(name) {
                clash_upper.clone()
            } else {
                clash_lower.clone()
            });
        }
        let at = json!({"line": line, "character": col});
        let place = format!("{entry}:{}:{} `{name}`", line + 1, col + 1);
        let prepared = client.request(
            "textDocument/prepareRename",
            json!({"textDocument": {"uri": entry_uri}, "position": at}),
        );
        let prepared = prepared.get("result").is_some_and(|r| !r.is_null());
        for new in names.iter().filter(|new| *new != name) {
            outcome.asked += 1;
            let resp = client.request(
                "textDocument/rename",
                json!({"textDocument": {"uri": entry_uri}, "position": at, "newName": new}),
            );
            let changes = resp
                .pointer("/result/changes")
                .and_then(Value::as_object)
                .filter(|changes| !changes.is_empty());
            let Some(changes) = changes else {
                outcome.refused += 1;
                continue;
            };
            if !prepared {
                outcome.broken.push(format!(
                    "{place} -> {new}: prepareRename refused, rename edits"
                ));
                continue;
            }
            // The edits: file, start byte, end byte, new text.
            let mut edits: BTreeSet<(String, usize, usize, String)> = BTreeSet::new();
            let mut problem = None;
            let mut hits_self = false;
            for (uri, list) in changes {
                let rel = all_files
                    .iter()
                    .find(|f| uri_of(&root.join(f)) == *uri)
                    .map(|f| f.to_string_lossy().replace('\\', "/"));
                let Some(rel) = rel else {
                    problem = Some(format!("an edit outside the case: {uri}"));
                    continue;
                };
                let file_text = std::fs::read_to_string(root.join(&rel)).unwrap();
                for edit in list.as_array().into_iter().flatten() {
                    let pos =
                        |end: &str, field: &str| edit["range"][end][field].as_u64().unwrap_or(0);
                    let start =
                        offset_of(&file_text, pos("start", "line"), pos("start", "character"));
                    let end = offset_of(&file_text, pos("end", "line"), pos("end", "character"));
                    let old = file_text.get(start..end).unwrap_or("");
                    // A punned field is renamed by writing the field
                    // out: `x` becomes `x: new`.
                    let new_text = edit["newText"].as_str().unwrap_or("");
                    let plain = old == name && new_text == new.as_str();
                    let unpunned = old == name && new_text == format!("{name}: {new}");
                    if !plain && !unpunned {
                        problem = Some(format!(
                            "the edit {rel}:{}:{} replaces `{old}` by `{new_text}`",
                            pos("start", "line") + 1,
                            pos("start", "character") + 1
                        ));
                    }
                    if rel == entry
                        && pos("start", "line") == *line as u64
                        && pos("start", "character") == *col as u64
                    {
                        hits_self = true;
                    }
                    edits.insert((rel.clone(), start, end, new_text.to_string()));
                }
            }
            if problem.is_none() && !hits_self {
                problem = Some("the place asked at is not edited".to_string());
            }
            if let Some(problem) = problem {
                outcome.broken.push(format!("{place} -> {new}: {problem}"));
                continue;
            }
            // One comparison per distinct rename.
            let key = format!("{new} {edits:?}");
            if !seen.insert(key) {
                continue;
            }
            distinct += 1;
            if !distinct.is_multiple_of(every) {
                continue;
            }
            let out = scratch.join("r");
            let _ = std::fs::remove_dir_all(&out);
            copy_dir(&root, &out);
            for (uri, list) in changes {
                let rel = all_files
                    .iter()
                    .find(|f| uri_of(&root.join(f)) == *uri)
                    .unwrap();
                let mut file_text = std::fs::read_to_string(root.join(rel)).unwrap();
                let mut spans: Vec<(usize, usize, String)> = list
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|edit| {
                        let pos = |end: &str, field: &str| {
                            edit["range"][end][field].as_u64().unwrap_or(0)
                        };
                        (
                            offset_of(&file_text, pos("start", "line"), pos("start", "character")),
                            offset_of(&file_text, pos("end", "line"), pos("end", "character")),
                            edit["newText"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect();
                spans.sort();
                for (start, end, new_text) in spans.into_iter().rev() {
                    file_text.replace_range(start..end, &new_text);
                }
                std::fs::write(out.join(rel), file_text).unwrap();
            }
            outcome.renamed += 1;
            // (The lockfile of a package is written again for the copy.)
            silt(&out, &["check", run_entry]);
            let renamed = verdict(&out, &files, run_entry, run);
            let differs = base
                .iter()
                .zip(&renamed)
                .find(|(a, b)| same_name(a, name, new) != same_name(b, name, new));
            if let Some((_, after)) = differs {
                let first: String = after.lines().take(6).collect::<Vec<_>>().join("\n    ");
                outcome.broken.push(format!(
                    "{place} -> {new}: not the same program ({} edits in {:?}):\n    {first}",
                    changes
                        .values()
                        .map(|l| l.as_array().map_or(0, Vec::len))
                        .sum::<usize>(),
                    changes
                        .keys()
                        .map(|u| u.rsplit('/').next().unwrap_or(""))
                        .collect::<Vec<_>>(),
                ));
            }
        }
    }
    client.shutdown();
    let _ = std::fs::remove_dir_all(&scratch);
    outcome
}
