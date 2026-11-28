//! Claims the docs made that the language does not keep (audit of
//! 2026-10-19). Each one is a phrase that may not come back: the docs
//! and README are read with their line breaks and runs of spaces made
//! one space, so a claim is found however its prose is wrapped.

use std::path::{Path, PathBuf};

/// `README.md` and every `.md` under `docs/`, each with its text
/// in one line.
fn docs() -> Vec<(PathBuf, String)> {
    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![root.join("README.md")];
    collect(&root.join("docs"), &mut paths);
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (path, flat)
        })
        .collect()
}

fn assert_no_doc_says(claims: &[&str], why: &str) {
    let found: Vec<String> = docs()
        .iter()
        .flat_map(|(path, text)| {
            claims
                .iter()
                .filter(|claim| text.contains(*claim))
                .map(move |claim| format!("{}: {claim:?}", path.display()))
        })
        .collect();
    assert!(found.is_empty(), "{why}:\n{}", found.join("\n"));
}

/// silt has no float recovery operator: `1.0 / 0.0 else 0.0` does not
/// parse, and a division by zero is a runtime error.
#[test]
fn no_doc_lists_a_float_recovery_operator() {
    assert_no_doc_says(
        &["float recovery"],
        "a doc names an operator silt does not have",
    );
}

/// A binding named like a builtin module hides it in its scope, as any
/// binding hides an import of that name, and no warning is given.
#[test]
fn no_doc_promises_a_warning_for_shadowing_a_builtin_module() {
    assert_no_doc_says(
        &["emits a warning when a binding shadows"],
        "a doc promises a warning silt does not give",
    );
}

/// `channel.new` takes the capacity; `channel.new()` is a type error.
#[test]
fn no_doc_calls_channel_new_without_a_capacity() {
    assert_no_doc_says(
        &[
            "channel.new()",
            "takes zero or one argument",
            "with no arguments creates a true rendezvous channel",
        ],
        "a doc says channel.new can be called without a capacity",
    );
}

/// `import x` never picks one of a builtin module, a dependency and a
/// module of the package by an order: a dependency key named like
/// either of the others is an error, as a module named like a builtin
/// module is.
#[test]
fn no_doc_resolves_an_import_by_an_order() {
    assert_no_doc_says(
        &["`import x` in a module of a package means, in this order"],
        "a doc says an import picks among same-named modules by an order",
    );
}

/// A trait's method is called through the trait (`Show.show(x)`):
/// `x.show()` reads a field `show` of `x`, which the checker refuses.
#[test]
fn no_doc_calls_a_trait_method_on_its_value() {
    assert_no_doc_says(
        &["(`Show.show(left)`, `x.show()`)"],
        "a doc calls a trait's method with a dot after the value",
    );
}

/// `as` states a type and converts nothing: `-x as Float` of an Int
/// `x` is a type error, so it is no example of how `as` binds.
#[test]
fn no_doc_shows_as_converting_an_int() {
    assert_no_doc_says(
        &["`-x as Float` parses as"],
        "a doc shows `as` as if it made a Float of an Int",
    );
}

/// The failure of a stage nobody read is let go with its stream, and a
/// binding is let go at its last use: a program that ends normally
/// does not report it.
#[test]
fn no_doc_promises_a_report_for_a_stream_the_program_let_go() {
    assert_no_doc_says(
        &["where nobody reads its output any more, is an error when the program ends"],
        "a doc promises a report for an unread stage failure of a stream the program let go",
    );
}
