//! Every `silt` block of `README.md` and `docs/**/*.md` that parses is
//! as `silt fmt` writes it, except the blocks of `SKIP`. To fix a
//! failure, write the block the way the message shows it.
//!
//! A block is a program, or the statements of a function body: a block
//! that is not a program is formatted inside `fn main() { ... }`. A
//! block that parses neither way (it has `...` in it, or shows an
//! error) is not checked. A block in a list item is read without the
//! item's indentation.

use silt::source::FileId;

use crate::fmt_property::doc_snippets;

/// The blocks that stay as written, by file and a line each holds: their
/// subject is a spelling the formatter makes canonical (two equivalent
/// forms side by side) or where a line may break.
const SKIP: [(&str, &str); 6] = [
    ("docs/language/operators.md", "-- these are equivalent:"),
    (
        "docs/language/loops-and-pipes.md",
        "-- These are equivalent:",
    ),
    (
        "docs/language/operators.md",
        "-- OK: * never unary, unambiguous",
    ),
    ("docs/language/operators.md", "-- NOT a trailing closure"),
    ("docs/language/design-decisions.md", "-- OK: { on same line"),
    (
        "docs/language/pattern-matching.md",
        "  | \"Sunday\" -> \"weekend\"",
    ),
];

fn format(text: &str) -> Option<String> {
    silt::format::format(FileId::default(), text).ok()
}

/// `block` as a program, formatted.
fn as_program(block: &str) -> Option<String> {
    format(block)
}

/// `block` as the statements of a function body, formatted. None too
/// when the result cannot be taken out of the function line by line: a
/// string over several lines is not the printer's to indent.
fn as_statements(block: &str) -> Option<String> {
    if block.contains("\"\"\"") {
        return None;
    }
    let wrapped = format(&format!("fn main() {{\n{block}}}\n"))?;
    let inner = wrapped.strip_prefix("fn main() {\n")?.strip_suffix("}\n")?;
    let mut out = String::new();
    for line in inner.lines() {
        if !line.is_empty() {
            out.push_str(line.strip_prefix("  ")?);
        }
        out.push('\n');
    }
    (format(&format!("fn main() {{\n{out}}}\n"))? == wrapped).then_some(out)
}

/// How `silt fmt` writes `block`; None when it does not parse.
fn formatted(block: &str) -> Option<String> {
    let declares = block.lines().any(|line| {
        ["pub ", "fn ", "type ", "trait ", "import "]
            .iter()
            .any(|keyword| line.starts_with(keyword))
    });
    if declares {
        as_program(block).or_else(|| as_statements(block))
    } else {
        as_statements(block).or_else(|| as_program(block))
    }
}

/// `block` without the indentation all its lines share: a block in a
/// list item is indented as the item is.
fn dedented(block: &str) -> String {
    let indent_of = |line: &str| line.len() - line.trim_start_matches(' ').len();
    let indent = block
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(indent_of)
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    for line in block.lines() {
        out.push_str(line.get(indent..).unwrap_or(""));
        out.push('\n');
    }
    out
}

#[test]
fn every_doc_snippet_that_parses_is_canonical_formatted() {
    let snippets = doc_snippets();
    assert!(snippets.len() > 400, "only {} snippets", snippets.len());
    let mut failures = Vec::new();
    let mut skipped = [0usize; SKIP.len()];
    for snippet in &snippets {
        let file = snippet.name.rsplit_once(':').map_or("", |(file, _)| file);
        let skip = SKIP
            .iter()
            .position(|(skip_file, line)| file == *skip_file && snippet.text.contains(line));
        let text = dedented(&snippet.text);
        let Some(canonical) = formatted(&text) else {
            if let Some(entry) = skip {
                failures.push(format!(
                    "{}: does not parse, so it needs no entry in SKIP ({:?})",
                    snippet.name, SKIP[entry].1
                ));
            }
            continue;
        };
        match skip {
            Some(entry) if canonical == text => failures.push(format!(
                "{}: is formatted, so it needs no entry in SKIP ({:?})",
                snippet.name, SKIP[entry].1
            )),
            Some(entry) => skipped[entry] += 1,
            None if canonical != text => failures.push(format!(
                "{}: `silt fmt` writes it as\n{canonical}",
                snippet.name
            )),
            None => {}
        }
    }
    for (entry, count) in skipped.iter().enumerate() {
        if *count != 1 {
            failures.push(format!(
                "SKIP entry {:?} of {} names {count} blocks, not one",
                SKIP[entry].1, SKIP[entry].0
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} doc snippet(s) are not as `silt fmt` writes them:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
